use std::collections::BTreeMap;
use std::fs::File;
use std::fs::read_to_string;
use std::io::{self, BufRead};
use std::error::Error;
use std::path::{Path, PathBuf};
use serde_derive::{Serialize, Deserialize};
use toml;

pub fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where P: AsRef<Path>, {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

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
pub struct Ktestrc {
    pub ci_linux_repo:       PathBuf,
    pub ci_output_dir:       PathBuf,
    pub ci_branches_to_test: PathBuf,
}

pub fn ktestrc_read() -> Result<Ktestrc, Box<dyn Error>> {
    let config = read_to_string("/etc/ktest-ci.toml")?;
    let ktestrc: Ktestrc = toml::from_str(&config)?;

    Ok(ktestrc)
}

#[derive(Serialize, Deserialize, PartialEq, Copy, Clone)]
pub enum TestStatus {
    InProgress,
    Passed,
    Failed,
    NotRun,
    NotStarted,
    Unknown,
}

impl TestStatus {
    pub fn from_str(status: &str) -> TestStatus {
        if status.is_empty() {
            TestStatus::InProgress
        } else if status.contains("IN PROGRESS") {
            TestStatus::InProgress
        } else if status.contains("PASSED") {
            TestStatus::Passed
        } else if status.contains("FAILED") {
            TestStatus::Failed
        } else if status.contains("NOTRUN") {
            TestStatus::NotRun
        } else if status.contains("NOT STARTED") {
            TestStatus::NotStarted
        } else {
            TestStatus::Unknown
        }
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            TestStatus::InProgress  => "In progress",
            TestStatus::Passed      => "Passed",
            TestStatus::Failed      => "Failed",
            TestStatus::NotRun      => "Not run",
            TestStatus::NotStarted  => "Not started",
            TestStatus::Unknown     => "Unknown",
        }
    }

    pub fn table_class(&self) -> &'static str {
        match self {
            TestStatus::InProgress  => "table-secondary",
            TestStatus::Passed      => "table-success",
            TestStatus::Failed      => "table-danger",
            TestStatus::NotRun      => "table-secondary",
            TestStatus::NotStarted  => "table-secondary",
            TestStatus::Unknown     => "table-secondary",
        }
    }
}

#[derive(Serialize, Deserialize, Copy, Clone)]
pub struct TestResult {
    pub status:     TestStatus,
    pub duration:   usize,
}

pub type TestResultsMap = BTreeMap<String, TestResult>;

#[derive(Serialize, Deserialize)]
pub struct TestResults {
    pub d:          TestResultsMap
}

pub fn read_test_result(testdir: &std::fs::DirEntry) -> Option<TestResult> {
    Some(TestResult {
        status:     TestStatus::from_str(&read_to_string(&testdir.path().join("status")).ok()?),
        duration:   read_to_string(&testdir.path().join("duration")).unwrap_or("0".to_string()).parse().unwrap_or(0),
    })
}

pub fn commitdir_get_results(ktestrc: &Ktestrc, commit_id: &String) -> TestResultsMap {
    let mut results = BTreeMap::new();

    let results_dir = ktestrc.ci_output_dir.join(commit_id).read_dir();

    if let Ok(results_dir) = results_dir {
        for d in results_dir.filter_map(|i| i.ok()) {
            if let Some(r) = read_test_result(&d) {
                results.insert(d.file_name().into_string().unwrap(), r);
            }
        }
    }

    results
}

pub fn commitdir_get_results_toml(ktestrc: &Ktestrc, commit_id: &String) -> Result<TestResultsMap, Box<dyn Error>> {
    let toml = read_to_string(ktestrc.ci_output_dir.join(commit_id.to_owned() + ".toml"))?;
    let r: TestResults = toml::from_str(&toml)?;
    Ok(r.d)
}
