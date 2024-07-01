use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, create_dir_all, read_to_string};
use std::io::ErrorKind;
use std::io::prelude::*;
use std::path::PathBuf;
use std::time::SystemTime;
use die::die;
use serde_derive::Deserialize;
use toml;
use anyhow;

pub mod testresult_capnp;
pub mod worker_capnp;
pub mod testjob_capnp;

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
    pub max_commits:        u64,
    pub priority:           u64,
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

pub fn ktestrc_read() -> anyhow::Result<Ktestrc> {
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

#[derive(Copy, Clone, Debug)]
pub struct TestResult {
    pub status:     TestStatus,
    pub starttime:  DateTime<Utc>,
    pub duration:   u64,
}

pub type TestResultsMap = BTreeMap<String, TestResult>;

fn commitdir_get_results_fs(ktestrc: &Ktestrc, commit_id: &String) -> TestResultsMap {
    fn read_test_result(testdir: &std::fs::DirEntry) -> Option<TestResult> {
        let mut f = File::open(&testdir.path().join("status")).ok()?;
        let mut status = String::new();
        f.read_to_string(&mut status).ok()?;

        Some(TestResult {
            status:     TestStatus::from_str(&status),
            starttime:  f.metadata().ok()?.modified().ok()?.into(),
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

fn results_to_capnp(ktestrc: &Ktestrc, commit_id: &String, results_in: &TestResultsMap) -> anyhow::Result<()> {
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

pub fn commitdir_get_results(ktestrc: &Ktestrc, commit_id: &String) -> anyhow::Result<TestResultsMap> {
    let f = std::fs::read(ktestrc.output_dir.join(commit_id.to_owned() + ".capnp"))?;

    let message_reader = serialize::read_message_from_flat_slice(&mut &f[..], capnp::message::ReaderOptions::new())?;
    let entries = message_reader.get_root::<test_results::Reader>()?
        .get_entries()?;

    let mut results = BTreeMap::new();
    for e in entries {
        let r = TestResult {
            status:     e.get_status()?,
            starttime:  Utc.timestamp_opt(e.get_starttime(), 0).unwrap(),
            duration:   e.get_duration()
        };

        results.insert(e.get_name()?.to_string()?, r);
    }

    Ok(results)
}

use chrono::{DateTime, TimeZone, Utc};

#[derive(Debug)]
pub struct Worker {
    pub hostname:       String,
    pub workdir:        String,
    pub starttime:      DateTime<Utc>,
    pub branch:         String,
    pub age:            u64,
    pub commit:	        String,
    pub tests:          String,
}

pub type Workers = Vec<Worker>;

use worker_capnp::workers;

fn workers_parse(f: Vec<u8>) -> anyhow::Result<Workers> {
    let message_reader = serialize::read_message_from_flat_slice(&mut &f[..], capnp::message::ReaderOptions::new())?;
    let entries = message_reader.get_root::<workers::Reader>()?
        .get_entries()?;

    let workers = entries.iter().map(|e| Worker {
        hostname:   e.get_hostname().unwrap().to_string().unwrap(),
        workdir:    e.get_workdir().unwrap().to_string().unwrap(),
        starttime:  Utc.timestamp_opt(e.get_starttime(), 0).unwrap(),
        branch:     e.get_branch().unwrap().to_string().unwrap(),
        commit:     e.get_commit().unwrap().to_string().unwrap(),
        age:        e.get_age(),
        tests:      e.get_tests().unwrap().to_string().unwrap(),
    }).collect();

    Ok(workers)
}

pub fn workers_get(ktestrc: &Ktestrc) -> anyhow::Result<Workers> {
    let f = std::fs::read(ktestrc.output_dir.join("workers.capnp"))?;

    workers_parse(f)
}

use file_lock::{FileLock, FileOptions};

pub fn workers_update(ktestrc: &Ktestrc, n: Worker) -> Option<()> {
    let fname = ktestrc.output_dir.join("workers.capnp");
    let foptions = FileOptions::new().read(true).write(true).append(false).create(true);

    let mut filelock = FileLock::lock(fname, true, foptions)
        .map_err(|e| eprintln!("error locking workers: {}", e)).ok()?;

    let mut f = Vec::new();
    filelock.file.read_to_end(&mut f).ok()?;

    let mut workers: Workers  = workers_parse(f)
        .map_err(|e| eprintln!("error parsing workers: {}", e))
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.hostname != n.hostname || w.workdir != n.workdir)
        .collect();

    workers.push(n);

    let mut message = capnp::message::Builder::new_default();
    let workers_message = message.init_root::<workers::Builder>();
    let mut workers_list = workers_message.init_entries(workers.len().try_into().unwrap());

    for (idx, src) in workers.iter().enumerate() {
        let mut dst = workers_list.reborrow().get(idx.try_into().unwrap());

        dst.set_hostname(&src.hostname);
        dst.set_workdir(&src.workdir);
        dst.set_starttime(src.starttime.timestamp());
        dst.set_branch(&src.branch);
        dst.set_commit(&src.commit);
        dst.set_age(src.age);
        dst.set_tests(&src.tests);
    }

    filelock.file.set_len(0).ok()?;
    filelock.file.rewind().ok()?;

    serialize::write_message(&mut filelock.file, &message)
        .map_err(|e| eprintln!("error writing workers: {}", e)).ok()?;

    Some(())
}

pub fn update_lcov(rc: &Ktestrc, commit_id: &String) -> Option<()> {
    let commit_dir = rc.output_dir.join(commit_id);

    if !std::fs::remove_file(commit_dir.join("lcov-stale")).is_ok() { return Some(()); }

    let lockfile = "/home/testdashboard/linux-1-lock";
    let filelock = FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true)).ok()?;

    let mut args = Vec::new();

    let new_lcov: Vec<_> = std::fs::read_dir(&commit_dir).ok()?
        .filter_map(|d| d.ok())
        .filter_map(|d| d.file_name().into_string().ok())
        .filter(|d| d.starts_with("lcov.partial."))
        .collect();

    for d in &new_lcov {
        args.push("--add-tracefile".to_string());
        args.push(d.clone());
    }

    if commit_dir.join("lcov.info").exists() {
        args.push("--add-tracefile".to_string());
        args.push("lcov.info".to_string());
    }

    let status = std::process::Command::new("lcov")
        .current_dir(&commit_dir)
        .arg("--quiet")
        .arg("--output-file")
        .arg("lcov.info.new")
        .args(args)
        .status()
        .expect(&format!("failed to execute lcov"));
    if !status.success() {
        eprintln!("lcov error: {}", status);
        return Some(());
    }

    std::fs::rename(commit_dir.join("lcov.info.new"), commit_dir.join("lcov.info")).ok()?;

    for d in &new_lcov { std::fs::remove_file(commit_dir.join(d)).ok(); }

    let status = std::process::Command::new("git")
        .current_dir("/home/testdashboard/linux-1")
        .arg("checkout")
        .arg("-f")
        .arg(commit_id)
        .status()
        .expect(&format!("failed to execute genhtml"));
    if !status.success() {
        eprintln!("git checkout error: {}", status);
        return Some(());
    }

    let status = std::process::Command::new("genhtml")
        .current_dir("/home/testdashboard/linux-1")
        .arg("--output-directory")
        .arg(commit_dir.join("lcov"))
        .arg(commit_dir.join("lcov.info"))
        .status()
        .expect(&format!("failed to execute genhtml"));
    if !status.success() {
        eprintln!("genhtml error: {}", status);
        return Some(());
    }

    drop(filelock);
    Some(())
}

pub fn lockfile_exists(rc: &Ktestrc, commit: &str, test_name: &str, create: bool) -> bool {
    let lockfile = rc.output_dir.join(commit).join(test_name).join("status");

    let timeout = std::time::Duration::from_secs(3600);
    let metadata = std::fs::metadata(&lockfile);

    if let Ok(metadata) = metadata {
        let elapsed = metadata.modified().unwrap()
            .elapsed()
            .unwrap_or(std::time::Duration::from_secs(0));

        if metadata.is_file() &&
           metadata.len() == 0 &&
           elapsed > timeout &&
           std::fs::remove_file(&lockfile).is_ok() {
            eprintln!("Deleted stale lock file {:?}, mtime {:?} now {:?} elapsed {:?})",
                      &lockfile, metadata.modified().unwrap(),
                      SystemTime::now(),
                      elapsed);
        }
    }

    if !create {
        lockfile.exists()
    } else {
        let dir = lockfile.parent().unwrap();
        let r = create_dir_all(dir);
        if let Err(e) = r {
            if e.kind() != ErrorKind::AlreadyExists {
                die!("error creating {:?}: {}", dir, e);
            }
        }

        let r = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lockfile);
        if let Err(ref e) = r {
            if e.kind() != ErrorKind::AlreadyExists {
                die!("error creating {:?}: {}", lockfile, e);
            }
        }

        r.is_ok()
    }
}
