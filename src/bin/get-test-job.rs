extern crate libc;
use std::path::Path;
use std::process;
use ci_cgi::{Ktestrc, ciconfig_read, lockfile_exists};
use ci_cgi::{Worker, workers_update};
use file_lock::{FileLock, FileOptions};
use chrono::Utc;
use clap::Parser;

#[derive(Debug)]
struct TestJob {
    branch:     String,
    commit:     String,
    age:        u64,
    test:       String,
    subtests:   Vec<String>,
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    dry_run:    bool,

    #[arg(short, long)]
    verbose:    bool,

    hostname:   String,
    workdir:    String,
}

use memmap::MmapOptions;
use std::fs::OpenOptions;
use std::str;

fn commit_test_matches(job: &Option<TestJob>, commit: &str, test: &str) -> bool {
    if let Some(job) = job {
        if job.commit == commit && job.test == test {
            return true;
        }
    }

    false
}

fn get_test_job(rc: &Ktestrc) -> Option<TestJob> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(rc.output_dir.join("jobs")).unwrap();
    let map = unsafe { MmapOptions::new().map(&file).unwrap() };

    let mut len = file.metadata().unwrap().len();
    if len == 0 {
        return None;
    }

    let mut ret = None;

    for job in map.rsplit(|b| *b == b'\n') {
        if job.is_empty() {
            continue;
        }

        let mut fields = job.split(|b| *b == b' ');
        let branch      = str::from_utf8(fields.next().unwrap()).unwrap();
        let commit      = str::from_utf8(fields.next().unwrap()).unwrap();
        let age_str     = str::from_utf8(fields.next().unwrap()).unwrap();
        let age         = str::parse::<u64>(age_str).unwrap();
        let test        = str::from_utf8(fields.next().unwrap()).unwrap();
        let subtest     = str::from_utf8(fields.next().unwrap()).unwrap();

        if ret.is_none() {
            ret = Some(TestJob {
                branch:     branch.to_string(),
                commit:     commit.to_string(),
                test:       test.to_string(),
                age,
                subtests:   vec![subtest.to_string()],
            });

            len = job.as_ptr() as u64 - map.as_ptr() as u64;
        } else if commit_test_matches(&ret, commit, test) {
            if let Some(ref mut r) = ret {
                r.subtests.push(subtest.to_string());
                len = job.as_ptr() as u64 - map.as_ptr() as u64;

                if r.subtests.len() > 20 {
                    break;
                }
            }
        } else {
            break;
        }
    }

    let _ = file.set_len(len);

    ret
}

fn subtest_full_name(test_path: &Path, subtest: &String) -> String {
    format!("{}.{}",
            test_path.file_stem().unwrap().to_string_lossy(),
            subtest.replace("/", "."))
}

fn create_job_lockfiles(rc: &Ktestrc, mut job: TestJob) -> Option<TestJob> {
    job.subtests = job.subtests.iter()
        .filter(|i| lockfile_exists(rc, &job.commit,
                                    &subtest_full_name(&Path::new(&job.test), &i), true))
        .map(|i| i.to_string())
        .collect();

    if !job.subtests.is_empty() { Some(job) } else { None }
}

fn get_and_lock_job(rc: &Ktestrc) -> Option<TestJob> {
    loop {
        let job = get_test_job(rc);
        if let Some(job) = job {
            let job = create_job_lockfiles(rc, job);
            if job.is_some() {
                return job;
            }
        } else {
            return job;
        }

    }
}

fn main() {
    let args = Args::parse();

    let rc = ciconfig_read();
    if let Err(e) = rc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let rc = rc.unwrap();
    let rc = rc.ktest;

    let lockfile = rc.output_dir.join("jobs.lock");
    let filelock = FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true)).unwrap();

    let job = if !args.dry_run {
        get_and_lock_job(&rc)
    } else {
        get_test_job(&rc)
    };

    drop(filelock);

    if let Some(job) = job {
        let tests = job.test + " " + &job.subtests.join(" ");

        println!("TEST_JOB {} {} {}", job.branch, job.commit, tests);

        workers_update(&rc, Worker {
            hostname:   args.hostname,
            workdir:    args.workdir,
            starttime:  Utc::now(),
            branch:     job.branch.clone(),
            age:        job.age,
            commit:     job.commit.clone(),
            tests:      tests.clone(),
        });
    } else {
        workers_update(&rc, Worker {
            hostname:   args.hostname,
            workdir:    args.workdir,
            starttime:  Utc::now(),
            branch:     "".to_string(),
            age:        0,
            commit:     "".to_string(),
            tests:      "".to_string(),
        });
    }
}
