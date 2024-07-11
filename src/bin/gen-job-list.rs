extern crate libc;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Stdio;
use ci_cgi::{CiConfig, Userrc, RcTestGroup, ciconfig_read, git_get_commit, commitdir_get_results, lockfile_exists, commit_update_results_from_fs, subtest_full_name};
use ci_cgi::TestResultsMap;
use file_lock::{FileLock, FileOptions};
use memoize::memoize;
use anyhow;
use chrono::Utc;
use clap::Parser;

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

fn branch_test_jobs(rc: &CiConfig, repo: &git2::Repository,
                    branch: &str,
                    test_group: &RcTestGroup,
                    test_path: &Path,
                    verbose: bool) -> Vec<TestJob> {
    let full_test_path = rc.ktest.ktest_dir.join("tests").join(test_path);
    let mut ret = Vec::new();

    let subtests = get_subtests(full_test_path);

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

    let mut commits_updated = HashSet::new();

    for (age, commit) in walk
            .filter_map(|i| i.ok())
            .filter_map(|i| repo.find_commit(i).ok())
            .take(test_group.max_commits as usize)
            .enumerate() {
        let commit = commit.id().to_string();

        let results = commitdir_get_results(&rc.ktest, &commit).unwrap_or(BTreeMap::new());

        if verbose { eprintln!("at commit {} age {}\nresults {:?}",
            &commit, age, results) }

        let missing_subtests: Vec<_> = subtests
            .iter()
            .filter(|i| {
                let full_subtest_name = subtest_full_name(&test_path, &i);

                !have_result(&results, &full_subtest_name) &&
                    !lockfile_exists(&rc.ktest, &commit, &full_subtest_name,
                                     false,
                                     &mut commits_updated)
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

    for i in commits_updated.iter() {
        commit_update_results_from_fs(&rc.ktest, &i);
    }

    ret
}

fn user_test_jobs(rc: &CiConfig, repo: &git2::Repository,
                  user: &Userrc,
                  verbose: bool) -> Vec<TestJob> {
    let mut ret: Vec<_> = user.branch.iter()
        .flat_map(move |(branch, branchconfig)| branchconfig.tests.iter()
            .filter_map(|i| user.test_group.get(i)).map(move |testgroup| (branch, testgroup)))
        .flat_map(move |(branch, testgroup)| testgroup.tests.iter()
            .flat_map(move |test| branch_test_jobs(rc, repo, &branch, &testgroup, &test, verbose)))
        .collect();

    /* sort by commit, dedup */

    ret.sort();
    ret.reverse();
    ret
}

fn rc_test_jobs(rc: &CiConfig, repo: &git2::Repository,
                verbose: bool) -> Vec<TestJob> {
    let mut ret: Vec<_> = rc.users.iter()
        .flat_map(|(_, user)| user_test_jobs(rc, repo, &user, verbose))
        .collect();

    /* sort by commit, dedup */

    ret.sort();
    ret.reverse();
    ret
}

fn write_test_jobs(rc: &CiConfig, jobs_in: Vec<TestJob>, verbose: bool) -> anyhow::Result<()> {
    eprintln!("Writing {} test jobs...", jobs_in.len());

    if verbose {
        eprint!("jobs: {:?}", jobs_in);
    }

    let jobs_fname      = rc.ktest.output_dir.join("jobs");
    let jobs_fname_new  = rc.ktest.output_dir.join("jobs.new");
    let mut jobs_out    = std::io::BufWriter::new(File::create(&jobs_fname_new)?);

    for job in jobs_in.iter() {
        for subtest in job.subtests.iter() {
            jobs_out.write(job.branch.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.commit.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.age.to_string().as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.test.as_os_str().as_encoded_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(subtest.as_bytes())?;
            jobs_out.write(b"\n")?;
        }
    }

    jobs_out.flush()?;
    drop(jobs_out);
    std::fs::rename(jobs_fname_new, jobs_fname)?;
    Ok(())
}

fn fetch_remotes(rc: &CiConfig, repo: &git2::Repository) -> anyhow::Result<bool> {
    fn fetch_remotes_locked(rc: &CiConfig, repo: &git2::Repository) -> Result<(), git2::Error> {
        for (_, userconfig) in &rc.users {
            for (branch, branchconfig) in &userconfig.branch {
                let fetch = branchconfig.fetch
                    .split_whitespace()
                    .map(|i| OsStr::new(i));

                let status = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&rc.ktest.linux_repo)
                    .arg("fetch")
                    .args(fetch)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .status()
                    .expect(&format!("failed to execute fetch"));
                if !status.success() {
                    eprintln!("fetch error for {}: {}", branchconfig.fetch, status);
                    continue;
                }

                let fetch_head = repo.revparse_single("FETCH_HEAD")
                    .map_err(|e| { eprintln!("error parsing FETCH_HEAD: {}", e); e})?
                    .peel_to_commit()
                    .map_err(|e| { eprintln!("error getting FETCH_HEAD: {}", e); e})?;

                repo.branch(branch, &fetch_head, true)?;
            }
        }

        Ok(())
    }

    let lockfile = rc.ktest.output_dir.join("fetch.lock");
    let metadata = std::fs::metadata(&lockfile);
    if let Ok(metadata) = metadata {
        let elapsed = metadata.modified().unwrap()
            .elapsed()
            .unwrap_or_default();

        if elapsed < std::time::Duration::from_secs(30) {
            return Ok(false);
        }
    }

    let mut filelock = FileLock::lock(lockfile, false, FileOptions::new().create(true).write(true))?;

    eprint!("Fetching remotes...");
    fetch_remotes_locked(rc, repo)?;
    eprintln!(" done");

    filelock.file.write_all(b"ok")?; /* update lockfile mtime */

    /*
     * XXX: return true only if remotes actually changed
     */
    Ok(true)
}

fn update_jobs(rc: &CiConfig, repo: &git2::Repository, verbose: bool) -> anyhow::Result<()> {
    if !fetch_remotes(rc, repo)? {
        eprintln!("remotes unchanged, skipping updating joblist");
        return Ok(());
    }

    let lockfile = rc.ktest.output_dir.join("jobs.lock");
    let filelock = FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true))?;

    let jobs_in = rc_test_jobs(rc, repo, false);
    write_test_jobs(rc, jobs_in, verbose)?;

    drop(filelock);

    Ok(())
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    verbose:    bool,
}

fn main() {
    let args = Args::parse();

    let rc = ciconfig_read();
    if let Err(e) = rc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let rc = rc.unwrap();

    let repo = git2::Repository::open(&rc.ktest.linux_repo);
    if let Err(e) = repo {
        eprintln!("Error opening {:?}: {}", rc.ktest.linux_repo, e);
        eprintln!("Please specify correct linux_repo");
        process::exit(1);
    }
    let repo = repo.unwrap();

    if let Err(e) = update_jobs(&rc, &repo, args.verbose) {
        eprintln!("update_jobs() error: {}", e);
    }
}
