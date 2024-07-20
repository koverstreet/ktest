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

use ci_cgi::durations_capnp::durations;
use capnp::serialize;

fn commit_test_matches(job: &Option<TestJob>, commit: &str, test: &str) -> bool {
    if let Some(job) = job {
        if job.commit == commit && job.test == test {
            return true;
        }
    }

    false
}

fn test_duration(durations: Option<&[u8]>, test: &str, subtest: &str) -> Option<u64> {

    if let Some(d) = durations {
        let mut d = d;
        let d_reader = serialize::read_message_from_flat_slice(&mut d, capnp::message::ReaderOptions::new()).ok();
        let d = d_reader.as_ref().map(|x| x.get_root::<durations::Reader>().ok()).flatten();
        if d.is_none() {
            return None;
        }
        let d = d.unwrap();

        let d = d.get_entries();
        if let Err(e) = d.as_ref() {
            eprintln!("error getting test duration entries: {}", e);
            return None;
        }
        let d = d.unwrap();

        let full_test = subtest_full_name(test, subtest);
        let full_test = full_test.as_str();

        let mut l = 0;
        let mut r = d.len();

        while l < r {
            let m = l + (r - l) / 2;
            let d_m = d.get(m);
            let d_m_test = d_m.get_test();

            // why does this happen? */
            if d_m_test.is_err() {
                eprintln!("no test at idx {}/){}", m, d.len());
                return None;
            }

            let d_m_test = d_m_test.unwrap().to_str().unwrap();

            use std::cmp::Ordering::*;
            match full_test.cmp(d_m_test) {
                Less    => r = m,
                Equal   => return Some(d_m.get_duration()),
                Greater => l = m,
            }
        }
    }

    None
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
                let duration_secs = test_duration(durations, test, subtest);

                if args.verbose {
                    println!("duration for {}.{}={:?}", test, subtest, duration_secs);
                }

                let duration_secs = duration_secs.unwrap_or(rc.subtest_duration_def);

                if duration_sum != 0 && duration_sum + duration_secs > rc.subtest_duration_max {
                    break;
                }

                duration_sum += duration_secs;
                r.subtests.push(subtest.to_string());
            }
        } else {
            break;
        }
    }

    if !args.dry_run {
        let _ = file.set_len(len);
    }

    ret
}

fn create_job_lockfiles(rc: &Ktestrc, mut job: TestJob) -> Option<TestJob> {
    let mut commits_updated = HashSet::new();

    job.subtests = job.subtests.iter()
        .filter(|i| lockfile_exists(rc, &job.commit,
                                    &subtest_full_name(&job.test, &i),
                                    true,
                                    &mut commits_updated))
        .map(|i| i.to_string())
        .collect();

    for i in commits_updated.iter() {
        commit_update_results_from_fs(rc, &i);
    }

    if !job.subtests.is_empty() { Some(job) } else { None }
}

fn get_and_lock_job(args: &Args, rc: &Ktestrc, durations: Option<&[u8]>) -> Option<TestJob> {
    loop {
        let job = get_test_job(args, rc, durations);
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

    let durations_file = File::open(rc.output_dir.join("test_durations.capnp")).ok();
    let durations_map = durations_file.map(|x| unsafe { MmapOptions::new().map(&x).ok() } ).flatten();
    let durations = durations_map.as_ref().map(|x| x.as_ref());

    let lockfile = rc.output_dir.join("jobs.lock");
    let filelock = FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true)).unwrap();

    let job = if !args.dry_run {
        get_and_lock_job(&args, &rc, durations)
    } else {
        get_test_job(&args, &rc, durations)
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

        commit_update_results_from_fs(&rc, &job.commit);
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
