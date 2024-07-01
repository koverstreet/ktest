extern crate libc;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Stdio;
use ci_cgi::{Ktestrc, KtestrcTestGroup, ktestrc_read, git_get_commit, commitdir_get_results, lockfile_exists};
use ci_cgi::TestResultsMap;
use file_lock::{FileLock, FileOptions};
use memoize::memoize;
use anyhow;
use chrono::Utc;

#[memoize]
fn get_subtests(test_path: PathBuf) -> Vec<String> {
    let output = std::process::Command::new(&test_path)
        .arg("list-tests")
        .output()
        .expect(&format!("failed to execute process {:?} ", &test_path))
        .stdout;
    let output = String::from_utf8_lossy(&output);

    output
        .split_whitespace()
        .map(|i| i.to_string())
        .collect()
}

#[derive(Debug)]
pub struct TestJob {
    branch:     String,
    commit:     String,
    age:        u64,
    priority:   u64,
    test:       PathBuf,
    subtests:   Vec<String>,
}

fn testjob_weight(j: &TestJob) -> u64 {
    j.age + j.priority
}

use std::cmp::Ordering;

impl Ord for TestJob {
    fn cmp(&self, other: &Self) -> Ordering {
        testjob_weight(self).cmp(&testjob_weight(other))
    }
}

impl PartialOrd for TestJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl PartialEq for TestJob {
    fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal }
}

impl Eq for TestJob {}

fn subtest_full_name(test_path: &Path, subtest: &String) -> String {
    format!("{}.{}",
            test_path.file_stem().unwrap().to_string_lossy(),
            subtest.replace("/", "."))
}

fn have_result(results: &TestResultsMap, subtest: &str) -> bool {
    use ci_cgi::TestStatus;

    let r = results.get(subtest);
    if let Some(r) = r {
        let elapsed = Utc::now() - r.starttime;
        let timeout = chrono::Duration::minutes(30);

        r.status != TestStatus::Inprogress || elapsed < timeout
    } else {
        false
    }
}

fn branch_test_jobs(rc: &Ktestrc, repo: &git2::Repository,
                    branch: &str,
                    test_group: &KtestrcTestGroup,
                    test_path: &Path,
                    verbose: bool) -> Vec<TestJob> {
    let test_path = rc.ktest_dir.join("tests").join(test_path);
    let mut ret = Vec::new();

    let subtests = get_subtests(test_path.clone());

    if verbose { eprintln!("looking for tests to run for branch {} test {:?} subtests {:?}",
        branch, test_path, subtests) }

    let mut walk = repo.revwalk().unwrap();
    let reference = git_get_commit(&repo, branch.to_string());
    if reference.is_err() {
        eprintln!("branch {} not found", branch);
        return ret;
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        eprintln!("Error walking {}: {}", branch, e);
        return ret;
    }

    for (age, commit) in walk
            .filter_map(|i| i.ok())
            .filter_map(|i| repo.find_commit(i).ok())
            .take(test_group.max_commits as usize)
            .enumerate() {
        let commit = commit.id().to_string();

        let results = commitdir_get_results(rc, &commit).unwrap_or(BTreeMap::new());

        if verbose { eprintln!("at commit {} age {}\nresults {:?}",
            &commit, age, results) }

        let missing_subtests: Vec<_> = subtests
            .iter()
            .filter(|i| {
                let full_subtest_name = subtest_full_name(&test_path, &i);

                !have_result(&results, &full_subtest_name) &&
                    !lockfile_exists(rc, &commit, &full_subtest_name, false)
            })
            .map(|i| i.clone())
            .collect();

        if !missing_subtests.is_empty() {
            ret.push(TestJob {
                branch:     branch.to_string(),
                commit:     commit.clone(),
                age:        age as u64,
                priority:   test_group.priority,
                test:       test_path.to_path_buf(),
                subtests:   missing_subtests,
            });
        }
    }

    ret
}

fn rc_test_jobs(rc: &Ktestrc, repo: &git2::Repository,
                verbose: bool) -> Vec<TestJob> {
    let mut ret: Vec<_> = rc.branch.iter()
        .flat_map(move |(branch, branchconfig)| branchconfig.tests.iter()
            .filter_map(|i| rc.test_group.get(i)).map(move |testgroup| (branch, testgroup)))
        .flat_map(move |(branch, testgroup)| testgroup.tests.iter()
            .flat_map(move |test| branch_test_jobs(rc, repo, &branch, &testgroup, &test, verbose)))
        .collect();

    ret.sort();
    ret
}

use ci_cgi::testjob_capnp::test_jobs;
use capnp::serialize;

fn test_jobs_to_capnp(rc: &Ktestrc, jobs_in: Vec<TestJob>) -> anyhow::Result<()> {
    let mut message = capnp::message::Builder::new_default();
    let jobs = message.init_root::<test_jobs::Builder>();
    let mut jobs_list = jobs.init_entries(jobs_in.len().try_into().unwrap());

    for (idx, job_in) in jobs_in.iter().enumerate() {
        let mut job = jobs_list.reborrow().get(idx.try_into().unwrap());

        job.set_branch(job_in.branch.clone());
        job.set_commit(job_in.commit.clone());
        job.set_age(job_in.age);
        job.set_priority(job_in.priority);
        job.set_test(job_in.test.to_str().unwrap().to_owned());

        let mut subtests = job.init_subtests(job_in.subtests.len().try_into().unwrap());
        for (idx, subtest_in) in job_in.subtests.iter().enumerate() {
            subtests.set(idx.try_into().unwrap(), subtest_in);
        }
    }

    let jobs_fname       = rc.output_dir.join("jobs.capnp");
    let jobs_fname_new   = rc.output_dir.join("jobs.capnp.new");

    let mut jobs_out = File::create(&jobs_fname_new)?;

    serialize::write_message(&mut jobs_out, &message)?;
    drop(jobs_out);
    std::fs::rename(jobs_fname_new, jobs_fname)?;
    Ok(())
}

fn fetch_remotes(rc: &Ktestrc, repo: &git2::Repository) -> Result<(), git2::Error> {
    for (branch, branchconfig) in &rc.branch {
        let fetch = branchconfig.fetch
            .split_whitespace()
            .map(|i| OsStr::new(i));

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&rc.linux_repo)
            .arg("fetch")
            .args(fetch)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .expect(&format!("failed to execute fetch"));
        if !status.success() {
            eprintln!("fetch error: {}", status);
            return Ok(());
        }

        let fetch_head = repo.revparse_single("FETCH_HEAD")
            .map_err(|e| { eprintln!("error parsing FETCH_HEAD: {}", e); e})?
            .peel_to_commit()
            .map_err(|e| { eprintln!("error getting FETCH_HEAD: {}", e); e})?;

        repo.branch(branch, &fetch_head, true)?;
    }

    Ok(())
}

fn update_jobs(rc: &Ktestrc, repo: &git2::Repository) -> anyhow::Result<()> {
    let lockfile = "update_jobs.lock";
    let metadata = std::fs::metadata(&lockfile);
    if let Ok(metadata) = metadata {
        let elapsed = metadata.modified().unwrap()
            .elapsed()
            .unwrap_or_default();

        if elapsed < std::time::Duration::from_secs(30) {
            return Ok(());
        }
    }

    let mut filelock = FileLock::lock(lockfile, false, FileOptions::new().create(true).write(true))?;

    eprint!("Fetching remotes...");
    fetch_remotes(rc, repo)?;
    eprintln!(" done");

    /*
     * XXX: we only need to regenerate test jobs if remotes were updated
     */

    let jobs_in = rc_test_jobs(rc, repo, false);
    test_jobs_to_capnp(rc, jobs_in)?;

    filelock.file.write_all(b"ok")?; /* update lockfile mtime */
    Ok(())
}

fn main() {
    let ktestrc = ktestrc_read();
    if let Err(e) = ktestrc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let ktestrc = ktestrc.unwrap();

    let repo = git2::Repository::open(&ktestrc.linux_repo);
    if let Err(e) = repo {
        eprintln!("Error opening {:?}: {}", ktestrc.linux_repo, e);
        eprintln!("Please specify correct linux_repo");
        process::exit(1);
    }
    let repo = repo.unwrap();

    update_jobs(&ktestrc, &repo).ok();
}
