extern crate libc;
use std::collections::HashSet;
use std::fs::File;
use std::process;
use ci_cgi::{Ktestrc, ciconfig_read, lockfile_exists, commit_update_results_from_fs, subtest_full_name};
use ci_cgi::{Worker, workers_update, test_stats};
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
        eprintln!("get-test-job: No test job available");
        return None;
    }

    let mut commits_updated = HashSet::new();

    let mut duration_sum: u64 = 0;

    let map = unsafe { MmapOptions::new().map(&file).unwrap() };
    let mut ret = None;

    for job in map.rsplit(|b| *b == b'\n') {
        let job = str::from_utf8(job).unwrap();

        if job.is_empty() {
            continue;
        }

        if args.verbose {
            eprintln!("get-test-job: considering {}", job);
        }

        let mut fields = job.split(' ');
        let branch      = fields.next().unwrap();
        let commit      = fields.next().unwrap();
        let age_str     = fields.next().unwrap();
        let age         = str::parse::<u64>(age_str).unwrap();
        let test        = fields.next().unwrap();
        let subtest     = fields.next().unwrap();

        if ret.is_some() && !commit_test_matches(&ret, commit, test) {
            if args.verbose {
                eprintln!("get-test-job: subtest from different test as previous, breaking");
            }
            break;
        }

        let stats = test_stats(durations, test, subtest);
        if args.verbose {
            eprintln!("get-test-job: stats for {}.{}={:?}", test, subtest, stats);
        }
        let duration_secs = if let Some(s) = stats {
            s.duration
        } else {
            rc.subtest_duration_def
        };

        if duration_sum != 0 && duration_sum + duration_secs > rc.subtest_duration_max {
            if args.verbose {
                eprintln!("get-test-job: have {} > {} seconds of work, breaking",
                          duration_sum + duration_secs, rc.subtest_duration_max);
            }
            break;
        }

        if !lockfile_exists(rc, &commit, &subtest_full_name(&test, &subtest),
                            !args.dry_run, &mut commits_updated) {
            if args.verbose {
                eprintln!("get-test-job: test {} already in progress", job);
            }
            continue;
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
        let r = file.set_len(len);
        if let Err(e) = r {
            eprintln!("get-test-job: error truncating jobs file: {}", e);
        }
    }

    for i in commits_updated.iter() {
        commit_update_results_from_fs(rc, &i);
    }

    if args.verbose {
        eprintln!("get-test-job: got {:?}", ret);
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
    let mut rc = rc.ktest;
    rc.verbose = std::cmp::max(rc.verbose, args.verbose);

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
