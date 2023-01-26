extern crate libc;
use std::collections::BTreeMap;
use std::fs::{OpenOptions, create_dir_all};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process;
use std::time::SystemTime;
use ci_cgi::{Ktestrc, ktestrc_read, git_get_commit, commitdir_get_results_toml};
use die::die;
use file_lock::{FileLock, FileOptions};
use memoize::memoize;

#[memoize]
fn get_subtests(test_path: PathBuf) -> Vec<String> {
    let output = std::process::Command::new(&test_path)
        .arg("list-tests")
        .output()
        .expect(&format!("failed to execute process {:?} ", test_path))
        .stdout;
    let output = String::from_utf8_lossy(&output);

    output
        .split_whitespace()
        .map(|i| i.to_string())
        .collect()
}

fn lockfile_exists(rc: &Ktestrc, commit: &str, test_name: &str, create: bool) -> bool {
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

struct TestJob {
    branch:     String,
    commit:     String,
    age:        usize,
    test:       PathBuf,
    subtests:   Vec<String>,
}

fn subtest_full_name(test_path: &Path, subtest: &String) -> String {
    format!("{}.{}",
            test_path.file_stem().unwrap().to_string_lossy(),
            subtest.replace("/", "."))
}

fn branch_get_next_test_job(rc: &Ktestrc, repo: &git2::Repository,
                            branch: &str,
                            test_path: &Path,
                            nr_commits: usize) -> Option<TestJob> {
    let test_path = rc.ktest_dir.join("tests").join(test_path);
    let mut ret =  TestJob {
        branch:     branch.to_string(),
        commit:     String::new(),
        age:        0,
        test:       test_path.to_path_buf(),
        subtests:   Vec::new(),
    };

    let subtests = get_subtests(test_path.clone());

    let mut walk = repo.revwalk().unwrap();
    let reference = git_get_commit(&repo, branch.to_string());
    if reference.is_err() {
        eprintln!("branch {} not found", branch);
        return None;
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        eprintln!("Error walking {}: {}", branch, e);
        return None;
    }

    for commit in walk
            .filter_map(|i| i.ok())
            .filter_map(|i| repo.find_commit(i).ok()) {
        let commit = commit.id().to_string();
        ret.commit = commit.clone();

        let results = commitdir_get_results_toml(rc, &commit).unwrap_or(BTreeMap::new());

        for subtest in subtests.iter() {
            let full_subtest_name = subtest_full_name(&test_path, &subtest);

            if results.get(&full_subtest_name).is_none() &&
               !lockfile_exists(rc, &commit, &full_subtest_name, false) {
                ret.subtests.push(subtest.to_string());
                if ret.subtests.len() > 20 {
                    break;
                }
            }
        }

        if !ret.subtests.is_empty() {
            return Some(ret);
        }

        ret.age += 1;
        if ret.age > nr_commits {
            break;
        }
    }

    None
}

fn get_best_test_job(rc: &Ktestrc, repo: &git2::Repository) -> Option<TestJob> {
    let mut ret: Option<TestJob> = None;

    for (branch, branchconfig) in &rc.branch {
        for testgroup in branchconfig.tests.iter().filter_map(|i| rc.test_group.get(i)) {
            for test in &testgroup.tests {
                let job = branch_get_next_test_job(rc, repo, &branch,
                        &test, testgroup.max_commits);

                let ret_age = ret.as_ref().map_or(std::usize::MAX, |x| x.age);
                let job_age = job.as_ref().map_or(std::usize::MAX, |x| x.age);

                if job_age < ret_age {
                    ret = job;
                }
            }
        }
    }

    ret
}

fn create_job_lockfiles(rc: &Ktestrc, mut job: TestJob) -> Option<TestJob> {
    job.subtests = job.subtests.iter()
        .filter(|i| lockfile_exists(rc, &job.commit,
                                    &subtest_full_name(&Path::new(&job.test), &i), true))
        .map(|i| i.to_string())
        .collect();

    if !job.subtests.is_empty() { Some(job) } else { None }
}

fn fetch_remotes_locked(rc: &Ktestrc, repo: &git2::Repository) -> Result<(), git2::Error> {
    for (branch, branchconfig) in &rc.branch {
        let remote_branch = branchconfig.branch.as_ref().unwrap();

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&rc.linux_repo)
            .arg("fetch")
            .arg(branchconfig.remote.as_str())
            .arg(remote_branch)
            .status()
            .expect(&format!("failed to execute fetch"));
        if !status.success() {
            die!("fetch error");
        }

        /*
        repo.remote_anonymous(branchconfig.remote.as_str())?
            .download(&[&remote_branch], None)
            .map_err(|e| { eprintln!("download error: {}", e); e})?;
        */

        let fetch_head = repo.revparse_single("FETCH_HEAD")
            .map_err(|e| { eprintln!("error parsing FETCH_HEAD: {}", e); e})?
            .peel_to_commit()
            .map_err(|e| { eprintln!("error getting FETCH_HEAD: {}", e); e})?;

        repo.branch(branch, &fetch_head, true)?;
    }

    Ok(())
}

fn fetch_remotes(rc: &Ktestrc, repo: &git2::Repository) -> Result<(), git2::Error> {
    let lockfile = ".git_fetch.lock";

    let metadata = std::fs::metadata(&lockfile);
    if let Ok(metadata) = metadata {
        let elapsed = metadata.modified().unwrap()
            .elapsed()
            .unwrap_or(std::time::Duration::from_secs(0));

        if elapsed < std::time::Duration::from_secs(30) {
            return Ok(());
        }
    }

    let filelock = FileLock::lock(lockfile, false, FileOptions::new().create(true).write(true));
    if filelock.is_ok() {
        fetch_remotes_locked(rc, repo)
    } else {
        Ok(())
    }
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

    fetch_remotes(&ktestrc, &repo)
        .map_err(|e| die!("error fetching remotes: {}", e)).ok();

    let mut job: Option<TestJob>;

    loop {
        job = get_best_test_job(&ktestrc, &repo);

        if job.is_none() {
            break;
        }

        job = create_job_lockfiles(&ktestrc, job.unwrap());
        if let Some(job) = job {
            print!("{} {} {}", job.branch, job.commit, job.test.display());
            for t in job.subtests {
                print!(" {}", t);
            }
            println!("");
            break;
        }
    }

}
