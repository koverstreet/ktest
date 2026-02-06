extern crate libc;
use chrono::Utc;
use ci_cgi::{
    ciconfig_read, commit_update_results_from_fs, lockfile_exists, subtest_full_name, CiConfig,
    Ktestrc,
};
use ci_cgi::{
    test_stats, user_stats_get, user_stats_select_fair, user_stats_update, workers_update, Worker,
};
use clap::Parser;
use file_lock::{FileLock, FileOptions};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::process;

#[derive(Debug)]
struct TestJob {
    user: String,
    branch: String,
    commit: String,
    age: u64,
    test: String,
    subtests: Vec<String>,
    expected_duration: u64,
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    dry_run: bool,

    #[arg(short, long)]
    verbose: bool,

    hostname: String,
    workdir: String,
}

use memmap::MmapOptions;
use std::str;

fn commit_test_matches(job: &Option<TestJob>, commit: &str, test: &str) -> bool {
    if let Some(job) = job {
        job.commit == commit && job.test == test
    } else {
        false
    }
}

/// Get list of users who have pending jobs (by looking for jobs.{user} files)
fn get_available_users(rc: &Ktestrc) -> Vec<String> {
    let mut users = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&rc.output_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("jobs.")
                && !name_str.ends_with(".new")
                && !name_str.ends_with(".lock")
            {
                if let Some(user) = name_str.strip_prefix("jobs.") {
                    // Check if file is non-empty
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.len() > 0 {
                            users.push(user.to_string());
                        }
                    }
                }
            }
        }
    }

    users
}

/// Get a test job from a specific user's queue
fn get_test_job_for_user(
    args: &Args,
    rc: &Ktestrc,
    durations: Option<&[u8]>,
    user: &str,
) -> Option<TestJob> {
    let jobs_file = rc.output_dir.join(format!("jobs.{}", user));

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&jobs_file)
        .ok()?;

    let mut len = file.metadata().ok()?.len();
    if len == 0 {
        if args.verbose {
            eprintln!("get-test-job: No jobs for user {}", user);
        }
        return None;
    }

    let mut commits_updated = HashSet::new();
    let mut duration_sum: u64 = 0;

    let map = unsafe { MmapOptions::new().map(&file).ok()? };
    let mut ret = None;

    for job in map.rsplit(|b| *b == b'\n') {
        let job = str::from_utf8(job).ok()?;

        if job.is_empty() {
            continue;
        }

        if args.verbose {
            eprintln!("get-test-job: considering {} for user {}", job, user);
        }

        let mut fields = job.split(' ');
        let branch = fields.next()?;
        let commit = fields.next()?;
        let age_str = fields.next()?;
        let age = str::parse::<u64>(age_str).ok()?;
        let test = fields.next()?;
        let subtest = fields.next()?;

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
                eprintln!(
                    "get-test-job: have {} > {} seconds of work, breaking",
                    duration_sum + duration_secs,
                    rc.subtest_duration_max
                );
            }
            break;
        }

        if !lockfile_exists(
            rc,
            &commit,
            &subtest_full_name(&test, &subtest),
            !args.dry_run,
            &mut commits_updated,
        ) {
            if args.verbose {
                eprintln!("get-test-job: test {} already in progress", job);
            }
            continue;
        }

        if let Some(ref mut r) = ret {
            r.subtests.push(subtest.to_string());
            r.expected_duration += duration_secs;
        } else {
            ret = Some(TestJob {
                user: user.to_string(),
                branch: branch.to_string(),
                commit: commit.to_string(),
                test: test.to_string(),
                age,
                subtests: vec![subtest.to_string()],
                expected_duration: duration_secs,
            });
        }

        duration_sum += duration_secs;
        len = job.as_ptr() as u64 - map.as_ptr() as u64;
    }

    if !args.dry_run && ret.is_some() {
        let r = file.set_len(len);
        if let Err(e) = r {
            eprintln!(
                "get-test-job: error truncating jobs file for {}: {}",
                user, e
            );
        }
    }

    for i in commits_updated.iter() {
        commit_update_results_from_fs(rc, &i);
    }

    if args.verbose {
        eprintln!("get-test-job: got {:?} for user {}", ret, user);
    }

    ret
}

/// Get a test job using fair scheduling across users
fn get_test_job(args: &Args, rc: &CiConfig, durations: Option<&[u8]>) -> Option<TestJob> {
    let available_users = get_available_users(&rc.ktest);

    if available_users.is_empty() {
        if args.verbose {
            eprintln!("get-test-job: No users with pending jobs");
        }
        return None;
    }

    if args.verbose {
        eprintln!("get-test-job: Available users: {:?}", available_users);
    }

    // Get user stats for fair scheduling
    let user_stats = user_stats_get(&rc.ktest).unwrap_or_default();

    if args.verbose {
        eprintln!("get-test-job: User stats: {:?}", user_stats);
    }

    // Select user fairly (lowest effective recent runtime = higher priority)
    let selected_user = user_stats_select_fair(&user_stats, &available_users, &rc.ktest)?;

    if args.verbose {
        eprintln!("get-test-job: Selected user: {}", selected_user);
    }

    // Try to get a job from the selected user
    // If that fails (e.g., all jobs already in progress), try other users
    if let Some(job) = get_test_job_for_user(args, &rc.ktest, durations, &selected_user) {
        return Some(job);
    }

    // Fallback: try other users in order of effective recent runtime
    for user in available_users.iter().filter(|u| *u != &selected_user) {
        if let Some(job) = get_test_job_for_user(args, &rc.ktest, durations, user) {
            return Some(job);
        }
    }

    None
}

fn main() {
    let args = Args::parse();

    let rc = ciconfig_read();
    if let Err(e) = rc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let mut rc = rc.unwrap();
    rc.ktest.verbose = std::cmp::max(rc.ktest.verbose, args.verbose);

    let durations_file = File::open(rc.ktest.output_dir.join("test_durations.capnp")).ok();
    let durations_map = durations_file
        .map(|x| unsafe { MmapOptions::new().map(&x).ok() })
        .flatten();
    let durations = durations_map.as_ref().map(|x| x.as_ref());

    let lockfile = rc.ktest.output_dir.join("jobs.lock");
    let filelock =
        FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true)).unwrap();

    let job = get_test_job(&args, &rc, durations);

    drop(filelock);

    if let Some(job) = job {
        let tests = job.test.clone() + " " + &job.subtests.join(" ");

        println!("TEST_JOB {} {} {}", job.branch, job.commit, tests);

        // Update user stats with expected duration for fair scheduling
        if !args.dry_run {
            user_stats_update(&rc.ktest, &job.user, job.expected_duration);
        }

        workers_update(
            &rc.ktest,
            Worker {
                hostname: args.hostname,
                workdir: args.workdir,
                starttime: Utc::now(),
                user: job.user.clone(),
                branch: job.branch.clone(),
                age: job.age,
                commit: job.commit.clone(),
                tests: tests.clone(),
            },
        );
    } else {
        workers_update(
            &rc.ktest,
            Worker {
                hostname: args.hostname,
                workdir: args.workdir,
                starttime: Utc::now(),
                user: "".to_string(),
                branch: "".to_string(),
                age: 0,
                commit: "".to_string(),
                tests: "".to_string(),
            },
        );
    }
}
