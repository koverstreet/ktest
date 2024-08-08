extern crate libc;
use std::collections::HashSet;
use std::fs::File;
use std::process;
use ci_cgi::{Ktestrc, ciconfig_read, lockfile_exists, commit_update_results_from_fs, subtest_full_name};
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

use ci_cgi::test_duration;

fn commit_test_matches(job: &Option<TestJob>, commit: &str, test: &str) -> bool {
    if let Some(job) = job {
        job.commit == commit && job.test == test
    } else {
        false
    }
}

fn get_test_job(args: &Args, rc: &Ktestrc, durations: Option<&[u8]>) -> Option<TestJob> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(rc.output_dir.join("jobs")).unwrap();
    let mut len = file.metadata().unwrap().len();
    if len == 0 {
        return None;
    }

    let mut commits_updated = HashSet::new();

    let mut duration_sum: u64 = 0;

    let map = unsafe { MmapOptions::new().map(&file).unwrap() };
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

        if ret.is_some() && !commit_test_matches(&ret, commit, test) {
            break;
        }

        let duration_secs = test_duration(durations, test, subtest);
        if args.verbose {
            println!("duration for {}.{}={:?}", test, subtest, duration_secs);
        }
        let duration_secs = duration_secs.unwrap_or(rc.subtest_duration_def);

        if duration_sum != 0 && duration_sum + duration_secs > rc.subtest_duration_max {
            break;
        }

        if !lockfile_exists(rc, &commit, &subtest_full_name(&test, &subtest),
                            !args.dry_run, &mut commits_updated) {
            break;
        }

        if let Some(ref mut r) = ret {
            r.subtests.push(subtest.to_string());
        } else {
            ret = Some(TestJob {
                branch:     branch.to_string(),
                commit:     commit.to_string(),
                test:       test.to_string(),
                age,
                subtests:   vec![subtest.to_string()],
            });
        }

        duration_sum += duration_secs;
        len = job.as_ptr() as u64 - map.as_ptr() as u64;
    }

    if !args.dry_run {
        let _ = file.set_len(len);
    }

    for i in commits_updated.iter() {
        commit_update_results_from_fs(rc, &i);
    }

    ret
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

    let durations_file = File::open(rc.output_dir.join("test_durations.capnp")).ok();
    let durations_map = durations_file.map(|x| unsafe { MmapOptions::new().map(&x).ok() } ).flatten();
    let durations = durations_map.as_ref().map(|x| x.as_ref());

    let lockfile = rc.output_dir.join("jobs.lock");
    let filelock = FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true)).unwrap();

    let job = get_test_job(&args, &rc, durations);

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
