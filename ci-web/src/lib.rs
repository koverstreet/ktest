use std::collections::BTreeMap;
use std::fs::read_to_string;
use std::fs::File;
use std::error::Error;
use std::path::PathBuf;
use serde_derive::Deserialize;
use toml;

pub mod testresult_capnp;

pub fn git_get_commit(repo: &git2::Repository, reference: String) -> Result<git2::Commit, git2::Error> {
    let r = repo.revparse_single(&reference);
    if let Err(e) = r {
        eprintln!("Error from resolve_reference_from_short_name {} in {}: {}", reference, repo.path().display(), e);
        return Err(e);
    }

    let r = r.unwrap().peel_to_commit();
    if let Err(e) = r {
        eprintln!("Error from peel_to_commit {} in {}: {}", reference, repo.path().display(), e);
        return Err(e);
    }
    r
}

#[derive(Deserialize)]
pub struct KtestrcTestGroup {
    pub max_commits:        usize,
    pub priority:           usize,
    pub tests:              Vec<PathBuf>,
}

#[derive(Deserialize)]
pub struct KtestrcBranch {
    pub fetch:              String,
    pub tests:              Vec<String>,
}

#[derive(Deserialize)]
pub struct Ktestrc {
    pub linux_repo:         PathBuf,
    pub output_dir:         PathBuf,
    pub ktest_dir:          PathBuf,
    pub test_group:         BTreeMap<String, KtestrcTestGroup>,
    pub branch:             BTreeMap<String, KtestrcBranch>,
}

pub fn ktestrc_read() -> Result<Ktestrc, Box<dyn Error>> {
    let config = read_to_string("/etc/ktest-ci.toml")?;
    let ktestrc: Ktestrc = toml::from_str(&config)?;

    Ok(ktestrc)
}

pub use testresult_capnp::test_result::Status as TestStatus;

impl TestStatus {
    fn from_str(status: &str) -> TestStatus {
        if status.is_empty() {
            TestStatus::Inprogress
        } else if status.contains("IN PROGRESS") {
            TestStatus::Inprogress
        } else if status.contains("PASSED") {
            TestStatus::Passed
        } else if status.contains("FAILED") {
            TestStatus::Failed
        } else if status.contains("NOTRUN") {
            TestStatus::Notrun
        } else if status.contains("NOT STARTED") {
            TestStatus::Notstarted
        } else {
            TestStatus::Unknown
        }
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            TestStatus::Inprogress  => "In progress",
            TestStatus::Passed      => "Passed",
            TestStatus::Failed      => "Failed",
            TestStatus::Notrun      => "Not run",
            TestStatus::Notstarted  => "Not started",
            TestStatus::Unknown     => "Unknown",
        }
    }

    pub fn table_class(&self) -> &'static str {
        match self {
            TestStatus::Inprogress  => "table-secondary",
            TestStatus::Passed      => "table-success",
            TestStatus::Failed      => "table-danger",
            TestStatus::Notrun      => "table-secondary",
            TestStatus::Notstarted  => "table-secondary",
            TestStatus::Unknown     => "table-secondary",
        }
    }
}

#[derive(Copy, Clone)]
pub struct TestResult {
    pub status:     TestStatus,
    pub duration:   u64,
}

pub type TestResultsMap = BTreeMap<String, TestResult>;

fn commitdir_get_results_fs(ktestrc: &Ktestrc, commit_id: &String) -> TestResultsMap {
    fn read_test_result(testdir: &std::fs::DirEntry) -> Option<TestResult> {
        Some(TestResult {
            status:     TestStatus::from_str(&read_to_string(&testdir.path().join("status")).ok()?),
            duration:   read_to_string(&testdir.path().join("duration")).unwrap_or("0".to_string()).parse().unwrap_or(0),
        })
    }

    let mut results = BTreeMap::new();

    let results_dir = ktestrc.output_dir.join(commit_id).read_dir();

    if let Ok(results_dir) = results_dir {
        for d in results_dir.filter_map(|i| i.ok()) {
            if let Some(r) = read_test_result(&d) {
                results.insert(d.file_name().into_string().unwrap(), r);
            }
        }
    }

    results
}

use testresult_capnp::test_results;
use capnp::serialize;

fn results_to_capnp(ktestrc: &Ktestrc, commit_id: &String, results_in: &TestResultsMap) -> Result<(), Box<dyn Error>> {
    let mut message = capnp::message::Builder::new_default();
    let results = message.init_root::<test_results::Builder>();
    let mut result_list = results.init_entries(results_in.len().try_into().unwrap());

    for (idx, (name, result_in)) in results_in.iter().enumerate() {
        let mut result = result_list.reborrow().get(idx.try_into().unwrap());

        result.set_name(name);
        result.set_duration(result_in.duration.try_into().unwrap());
        result.set_status(result_in.status);
    }

    let fname       = ktestrc.output_dir.join(commit_id.clone() + ".capnp");
    let fname_new   = ktestrc.output_dir.join(commit_id.clone() + ".capnp.new");

    let mut out = File::create(&fname_new)?;

    serialize::write_message(&mut out, &message)?;
    drop(out);
    std::fs::rename(fname_new, fname)?;

    Ok(())
}

pub fn commit_update_results_from_fs(ktestrc: &Ktestrc, commit_id: &String) {
    let results = commitdir_get_results_fs(&ktestrc, commit_id);

    results_to_capnp(ktestrc, commit_id, &results)
        .map_err(|e| eprintln!("error generating capnp: {}", e)).ok();
}

fn commit_get_results_capnp(ktestrc: &Ktestrc, commit_id: &String) -> Result<TestResultsMap, Box<dyn Error>> {
    let f = std::fs::read(ktestrc.output_dir.join(commit_id.to_owned() + ".capnp"))?;

    let message_reader = serialize::read_message_from_flat_slice(&mut &f[..], capnp::message::ReaderOptions::new())?;
    let entries = message_reader.get_root::<test_results::Reader>()?
        .get_entries()?;

    let mut results = BTreeMap::new();
    for e in entries {
        let r = TestResult {
            status:     e.get_status()?,
            duration:   e.get_duration()
        };

        results.insert(e.get_name()?.to_string(), r);
    }

    Ok(results)
}

pub fn commitdir_get_results(ktestrc: &Ktestrc, commit_id: &String) -> Result<TestResultsMap, Box<dyn Error>> {
    commit_get_results_capnp(ktestrc, commit_id)
}
