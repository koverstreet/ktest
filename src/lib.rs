use anyhow;
use die::die;
use serde_derive::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::fs::{create_dir_all, read_to_string, File, OpenOptions};
use std::io::prelude::*;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use toml;

pub mod branchlog_capnp;
pub mod durations_capnp;
pub mod testresult_capnp;
pub mod users;
pub mod worker_capnp;
pub use users::RcTestGroup;
pub use users::Userrc;

pub fn git_get_commit(
    repo: &git2::Repository,
    reference: String,
) -> Result<git2::Commit<'_>, git2::Error> {
    let r = repo.revparse_single(&reference);
    if let Err(e) = r {
        eprintln!(
            "Error from resolve_reference_from_short_name {} in {}: {}",
            reference,
            repo.path().display(),
            e
        );
        return Err(e);
    }

    let r = r.unwrap().peel_to_commit();
    if let Err(e) = r {
        eprintln!(
            "Error from peel_to_commit {} in {}: {}",
            reference,
            repo.path().display(),
            e
        );
        return Err(e);
    }
    r
}

#[derive(Deserialize)]
pub struct Ktestrc {
    pub linux_repo: PathBuf,
    pub output_dir: PathBuf,
    pub ktest_dir: PathBuf,
    #[serde(default)]
    pub ci_url: Option<String>,
    /// Git remote name for resolving branch refs (e.g. "bcachefs")
    #[serde(default)]
    pub ci_remote: Option<String>,
    /// SSH host for config pull/push (e.g. "evilpiepirate.org")
    #[serde(default)]
    pub ci_host: Option<String>,
    #[serde(default)]
    pub users_dir: Option<PathBuf>,
    #[serde(default)]
    pub subtest_duration_max: Option<u64>,
    #[serde(default)]
    pub subtest_duration_def: Option<u64>,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub user_nice: BTreeMap<String, i64>,
}

pub fn ktestrc_read() -> anyhow::Result<Ktestrc> {
    let config = read_to_string("/etc/ktest-ci.toml")?;
    let ktestrc: Ktestrc = toml::from_str(&config)?;

    Ok(ktestrc)
}

pub struct CiConfig {
    pub ktest: Ktestrc,
    pub users: BTreeMap<String, anyhow::Result<Userrc>>,
}

pub fn ciconfig_read() -> anyhow::Result<CiConfig> {
    let mut rc = CiConfig {
        ktest: ktestrc_read()?,
        users: BTreeMap::new(),
    };

    if let Some(ref users_dir) = rc.ktest.users_dir {
        for i in std::fs::read_dir(users_dir)?
            .filter_map(|x| x.ok())
            .map(|i| i.path())
        {
            rc.users.insert(
                i.file_stem().unwrap().to_string_lossy().to_string(),
                users::userrc_read(&i),
            );
        }
    }

    Ok(rc)
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
            TestStatus::Inprogress => "In progress",
            TestStatus::Passed => "Passed",
            TestStatus::Failed => "Failed",
            TestStatus::Notrun => "Not run",
            TestStatus::Notstarted => "Not started",
            TestStatus::Unknown => "Unknown",
        }
    }

    pub fn table_class(&self) -> &'static str {
        match self {
            TestStatus::Inprogress => "table-secondary",
            TestStatus::Passed => "table-success",
            TestStatus::Failed => "table-danger",
            TestStatus::Notrun => "table-secondary",
            TestStatus::Notstarted => "table-secondary",
            TestStatus::Unknown => "table-secondary",
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct TestResult {
    pub status: TestStatus,
    pub starttime: DateTime<Utc>,
    pub duration: u64,
}

pub type TestResultsMap = BTreeMap<String, TestResult>;

fn commitdir_get_results_fs(output_dir: &Path, commit_id: &str) -> TestResultsMap {
    fn read_test_result(testdir: &std::fs::DirEntry) -> Option<TestResult> {
        let mut f = File::open(&testdir.path().join("status")).ok()?;
        let mut status = String::new();
        f.read_to_string(&mut status).ok()?;

        Some(TestResult {
            status: TestStatus::from_str(&status),
            starttime: f.metadata().ok()?.modified().ok()?.into(),
            duration: read_to_string(&testdir.path().join("duration"))
                .unwrap_or("0".to_string())
                .parse()
                .unwrap_or(0),
        })
    }

    let mut results = BTreeMap::new();

    if let Ok(results_dir) = output_dir.join(commit_id).read_dir() {
        for d in results_dir.filter_map(|i| i.ok()) {
            if let Some(r) = read_test_result(&d) {
                results.insert(d.file_name().into_string().unwrap(), r);
            }
        }
    }

    results
}

use capnp::serialize;
use testresult_capnp::test_results;

fn results_to_capnp(
    output_dir: &Path,
    commit_id: &str,
    commit_message: Option<&str>,
    results_in: &TestResultsMap,
) -> anyhow::Result<()> {
    let mut message = capnp::message::Builder::new_default();
    let mut results = message.init_root::<test_results::Builder>();

    if let Some(msg) = commit_message {
        results.reborrow().set_message(msg);
    }
    results.reborrow().set_commit_id(commit_id);

    let mut result_list = results.init_entries(results_in.len().try_into().unwrap());

    for (idx, (name, result_in)) in results_in.iter().enumerate() {
        let mut result = result_list.reborrow().get(idx.try_into().unwrap());

        result.set_name(name);
        result.set_duration(result_in.duration.try_into().unwrap());
        result.set_status(result_in.status);
    }

    let fname = output_dir.join(format!("{commit_id}.capnp"));
    let fname_new = output_dir.join(format!("{commit_id}.capnp.new"));

    let mut out = File::create(&fname_new).map(std::io::BufWriter::new)?;
    serialize::write_message(&mut out, &message)?;
    out.into_inner()?;
    std::fs::rename(fname_new, fname)?;

    Ok(())
}

pub fn commit_update_results_from_fs(ktestrc: &Ktestrc, commit_id: &str) {
    commit_update_results(&ktestrc.output_dir, commit_id);
}

pub fn commit_update_results(output_dir: &Path, commit_id: &str) {
    let results = commitdir_get_results_fs(output_dir, commit_id);
    results_to_capnp(output_dir, commit_id, None, &results)
        .map_err(|e| eprintln!("error generating capnp: {}", e))
        .ok();
}

pub fn commit_update_results_with_message(
    output_dir: &Path,
    commit_id: &str,
    message: &str,
) {
    let results = commitdir_get_results_fs(output_dir, commit_id);
    results_to_capnp(output_dir, commit_id, Some(message), &results)
        .map_err(|e| eprintln!("error generating capnp: {}", e))
        .ok();
}

/// Rewrite an existing capnp file to add/update the commit message,
/// preserving the test results from the capnp (not re-reading from filesystem).
/// Used by migrate-capnp where commit dirs may have been GC'd.
pub fn commit_capnp_set_message(
    output_dir: &Path,
    commit_id: &str,
    message: &str,
) -> anyhow::Result<()> {
    let f = std::fs::read(output_dir.join(commit_id.to_owned() + ".capnp"))?;
    let existing = parse_test_results(&f)?;
    results_to_capnp(output_dir, commit_id, Some(message), &existing.tests)
}

pub struct CommitResultsCapnp {
    pub message: String,
    pub commit_id: String,
    pub tests: TestResultsMap,
}

fn parse_test_results(f: &[u8]) -> anyhow::Result<CommitResultsCapnp> {
    let message_reader =
        serialize::read_message_from_flat_slice(&mut &f[..], capnp::message::ReaderOptions::new())?;
    let root = message_reader.get_root::<test_results::Reader>()?;

    let message = root.get_message()
        .ok().and_then(|s| s.to_string().ok())
        .unwrap_or_default();
    let commit_id = root.get_commit_id()
        .ok().and_then(|s| s.to_string().ok())
        .unwrap_or_default();
    let entries = root.get_entries()?;

    let mut results = BTreeMap::new();
    for e in entries {
        let r = TestResult {
            status: e.get_status()?,
            starttime: Utc.timestamp_opt(e.get_starttime(), 0).unwrap(),
            duration: e.get_duration(),
        };

        results.insert(e.get_name()?.to_string()?, r);
    }

    Ok(CommitResultsCapnp {
        message,
        commit_id,
        tests: results,
    })
}

/// Conditional HTTP fetch of one capnp file; updates local cache.
/// Returns Ok(true) if data was fetched/cached, Ok(false) if 404.
fn fetch_capnp_cached(
    client: &reqwest::blocking::Client,
    base_url: &str,
    cache_dir: &Path,
    commit_id: &str,
) -> anyhow::Result<bool> {
    let cache_path = cache_dir.join(format!("{}.capnp", commit_id));
    let url = format!("{}/{}.capnp", base_url.trim_end_matches('/'), commit_id);

    let mut req = client.get(&url);

    // Conditional request if we have a cached copy
    if let Ok(meta) = std::fs::metadata(&cache_path) {
        if let Ok(mtime) = meta.modified() {
            let dt: DateTime<Utc> = mtime.into();
            req = req.header("If-Modified-Since",
                dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string());
        }
    }

    let resp = req.send()?;
    match resp.status().as_u16() {
        304 => Ok(true),
        200 => {
            let bytes = resp.bytes()?;
            let _ = std::fs::write(&cache_path, &bytes);
            Ok(true)
        }
        404 => Ok(false),
        s   => anyhow::bail!("HTTP {}: {}", s, url),
    }
}

/// Prefetch capnp files for multiple commits in parallel.
/// Uses HTTP/2 multiplexing over a shared client with 8 worker threads.
/// Commits already cached are skipped entirely (cache invalidation
/// happens via the freshness window on recent commits).
fn prefetch_capnp(base_url: &str, cache_dir: &Path, commit_ids: &[String]) {
    let _ = create_dir_all(cache_dir);

    // Split: first N_FRESH commits get conditional requests (might still be updating),
    // the rest only fetch if not cached at all.
    const N_FRESH: usize = 5;

    let to_fetch: Vec<(&str, bool)> = commit_ids.iter().enumerate()
        .filter_map(|(i, id)| {
            let cache_path = cache_dir.join(format!("{}.capnp", id));
            let cached = cache_path.exists();

            if i < N_FRESH {
                // Recent: always check (conditional request if cached)
                Some((id.as_str(), cached))
            } else if cached {
                None  // Old + cached: skip entirely
            } else {
                Some((id.as_str(), false))  // Old + not cached: fetch
            }
        })
        .collect();

    if to_fetch.is_empty() { return; }

    let client = reqwest::blocking::Client::new();
    let n_threads = 8.min(to_fetch.len()).max(1);
    let chunk_size = to_fetch.len().div_ceil(n_threads).max(1);

    std::thread::scope(|s| {
        for chunk in to_fetch.chunks(chunk_size) {
            let client = client.clone();
            s.spawn(move || {
                for &(id, _) in chunk {
                    if let Err(e) = fetch_capnp_cached(&client, base_url, cache_dir, id) {
                        eprintln!("warning: fetch {}: {}", &id[..12.min(id.len())], e);
                    }
                }
            });
        }
    });
}

fn commit_read_capnp(ktestrc: &Ktestrc, commit_id: &str) -> anyhow::Result<Vec<u8>> {
    Ok(std::fs::read(ktestrc.output_dir.join(format!("{}.capnp", commit_id)))?)
}

pub fn commitdir_get_results(ktestrc: &Ktestrc, commit_id: &str) -> anyhow::Result<TestResultsMap> {
    Ok(parse_test_results(&commit_read_capnp(ktestrc, commit_id)?)?.tests)
}

pub fn commitdir_get_results_full(ktestrc: &Ktestrc, commit_id: &str) -> anyhow::Result<CommitResultsCapnp> {
    // For single-commit access, ensure cache is fresh
    if let Some(ref base_url) = ktestrc.ci_url {
        let _ = create_dir_all(&ktestrc.output_dir);
        let client = reqwest::blocking::Client::new();
        fetch_capnp_cached(&client, base_url, &ktestrc.output_dir, commit_id)?;
    }
    parse_test_results(&commit_read_capnp(ktestrc, commit_id)?)
}

use chrono::{DateTime, TimeZone, Utc};

#[derive(Debug)]
pub struct Worker {
    pub hostname: String,
    pub workdir: String,
    pub starttime: DateTime<Utc>,
    pub user: String,
    pub branch: String,
    pub age: u64,
    pub commit: String,
    pub tests: String,
}

#[derive(Debug, Clone)]
pub struct UserStats {
    pub user: String,
    pub total_seconds: u64,  // all-time runtime
    pub recent_seconds: f64, // time-decayed recent runtime
    pub last_updated: DateTime<Utc>,
}

pub type AllUserStats = Vec<UserStats>;

/// Half-life for recent runtime decay (24 hours)
const RECENT_HALF_LIFE_SECS: f64 = 24.0 * 3600.0;

pub type Workers = Vec<Worker>;

use worker_capnp::workers;

fn workers_parse(f: Vec<u8>) -> anyhow::Result<Workers> {
    let message_reader =
        serialize::read_message_from_flat_slice(&mut &f[..], capnp::message::ReaderOptions::new())?;
    let entries = message_reader
        .get_root::<workers::Reader>()?
        .get_entries()?;

    let workers = entries
        .iter()
        .map(|e| Worker {
            hostname: e.get_hostname().unwrap().to_string().unwrap(),
            workdir: e.get_workdir().unwrap().to_string().unwrap(),
            starttime: Utc.timestamp_opt(e.get_starttime(), 0).unwrap(),
            user: e
                .get_user()
                .ok()
                .and_then(|s| s.to_string().ok())
                .unwrap_or_default(),
            branch: e.get_branch().unwrap().to_string().unwrap(),
            commit: e.get_commit().unwrap().to_string().unwrap(),
            age: e.get_age(),
            tests: e.get_tests().unwrap().to_string().unwrap(),
        })
        .collect();

    Ok(workers)
}

pub fn workers_get(ktestrc: &Ktestrc) -> anyhow::Result<Workers> {
    let f = std::fs::read(ktestrc.output_dir.join("workers.capnp"))?;

    workers_parse(f)
}

use file_lock::{FileLock, FileOptions};

pub fn workers_update(rc: &Ktestrc, n: Worker) -> Option<()> {
    if rc.verbose {
        eprintln!("workers_update: {:?}", n);
    }

    let fname = rc.output_dir.join("workers.capnp");
    let foptions = FileOptions::new()
        .read(true)
        .write(true)
        .append(false)
        .create(true);

    let mut filelock = FileLock::lock(fname, true, foptions)
        .map_err(|e| eprintln!("error locking workers: {}", e))
        .ok()?;

    let mut f = Vec::new();
    filelock.file.read_to_end(&mut f).ok()?;

    let mut workers: Workers = workers_parse(f)
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
        dst.set_user(&src.user);
        dst.set_branch(&src.branch);
        dst.set_commit(&src.commit);
        dst.set_age(src.age);
        dst.set_tests(&src.tests);
    }

    filelock.file.set_len(0).ok()?;
    filelock.file.rewind().ok()?;

    serialize::write_message(&mut filelock.file, &message)
        .map_err(|e| eprintln!("error writing workers: {}", e))
        .ok()?;

    Some(())
}

use worker_capnp::all_user_stats;

fn user_stats_parse(f: Vec<u8>) -> anyhow::Result<AllUserStats> {
    let message_reader =
        serialize::read_message_from_flat_slice(&mut &f[..], capnp::message::ReaderOptions::new())?;
    let entries = message_reader
        .get_root::<all_user_stats::Reader>()?
        .get_entries()?;

    let stats = entries
        .iter()
        .map(|e| UserStats {
            user: e.get_user().unwrap().to_string().unwrap(),
            total_seconds: e.get_total_seconds(),
            recent_seconds: e.get_recent_seconds(),
            last_updated: Utc.timestamp_opt(e.get_last_updated(), 0).unwrap(),
        })
        .collect();

    Ok(stats)
}

pub fn user_stats_get(rc: &Ktestrc) -> anyhow::Result<AllUserStats> {
    let f = std::fs::read(rc.output_dir.join("user_stats.capnp"))?;
    user_stats_parse(f)
}

/// Apply time decay to recent_seconds based on elapsed time
fn decay_recent(recent: f64, last_updated: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let elapsed_secs = (now - last_updated).num_seconds().max(0) as f64;
    let decay = (-elapsed_secs * std::f64::consts::LN_2 / RECENT_HALF_LIFE_SECS).exp();
    recent * decay
}

/// Get recent runtime for a user, with decay applied to current time
pub fn user_stats_recent(stats: &UserStats) -> f64 {
    decay_recent(stats.recent_seconds, stats.last_updated, Utc::now())
}

/// Update stats for a user when a job is handed out
pub fn user_stats_update(rc: &Ktestrc, user: &str, job_duration_secs: u64) -> Option<()> {
    let fname = rc.output_dir.join("user_stats.capnp");
    let foptions = FileOptions::new()
        .read(true)
        .write(true)
        .append(false)
        .create(true);

    let mut filelock = FileLock::lock(&fname, true, foptions)
        .map_err(|e| eprintln!("error locking user_stats: {}", e))
        .ok()?;

    let mut f = Vec::new();
    filelock.file.read_to_end(&mut f).ok()?;

    let mut stats: AllUserStats = if f.is_empty() {
        Vec::new()
    } else {
        user_stats_parse(f)
            .map_err(|e| eprintln!("error parsing user_stats: {}", e))
            .unwrap_or_default()
    };

    let now = Utc::now();
    let duration = job_duration_secs;

    if let Some(entry) = stats.iter_mut().find(|s| s.user == user) {
        // Decay recent_seconds to current time, then add new duration
        entry.recent_seconds =
            decay_recent(entry.recent_seconds, entry.last_updated, now) + duration as f64;
        entry.total_seconds += duration;
        entry.last_updated = now;
    } else {
        // New user
        stats.push(UserStats {
            user: user.to_string(),
            total_seconds: duration,
            recent_seconds: duration as f64,
            last_updated: now,
        });
    }

    // Write back
    let mut message = capnp::message::Builder::new_default();
    let stats_message = message.init_root::<all_user_stats::Builder>();
    let mut stats_list = stats_message.init_entries(stats.len().try_into().unwrap());

    for (idx, src) in stats.iter().enumerate() {
        let mut dst = stats_list.reborrow().get(idx.try_into().unwrap());
        dst.set_user(&src.user);
        dst.set_total_seconds(src.total_seconds);
        dst.set_recent_seconds(src.recent_seconds);
        dst.set_last_updated(src.last_updated.timestamp());
    }

    filelock.file.set_len(0).ok()?;
    filelock.file.rewind().ok()?;

    serialize::write_message(&mut filelock.file, &message)
        .map_err(|e| eprintln!("error writing user_stats: {}", e))
        .ok()?;

    Some(())
}

/// Select the user who should get the next job based on fairness (lowest effective recent runtime)
/// The nice value scales effective runtime: nice=0 is normal, nice=1 doubles effective runtime, etc.
pub fn user_stats_select_fair(
    stats: &AllUserStats,
    available_users: &[String],
    rc: &Ktestrc,
) -> Option<String> {
    let now = Utc::now();
    available_users
        .iter()
        .map(|u| {
            let recent = stats
                .iter()
                .find(|s| &s.user == u)
                .map(|s| decay_recent(s.recent_seconds, s.last_updated, now))
                .unwrap_or(0.0); // New users get priority

            // Apply nice: higher nice = higher effective runtime = lower priority
            // Clamp multiplier to prevent negative nice from causing division issues
            let nice = rc.user_nice.get(u).copied().unwrap_or(0);
            let multiplier = (1.0 + nice as f64).max(0.1);
            let effective = recent * multiplier;

            (u, effective)
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(u, _)| u.clone())
}

pub fn update_lcov(rc: &Ktestrc, commit_id: &str) -> Option<()> {
    let commit_dir = rc.output_dir.join(commit_id);

    if !std::fs::remove_file(commit_dir.join("lcov-stale")).is_ok() {
        return Some(());
    }

    let lockfile = "/home/testdashboard/linux-1-lock";
    let filelock =
        FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true)).ok()?;

    let mut args = Vec::new();

    let new_lcov: Vec<_> = std::fs::read_dir(&commit_dir)
        .ok()?
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

    std::fs::rename(
        commit_dir.join("lcov.info.new"),
        commit_dir.join("lcov.info"),
    )
    .ok()?;

    for d in &new_lcov {
        std::fs::remove_file(commit_dir.join(d)).ok();
    }

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

pub fn subtest_full_name(test: &str, subtest: &str) -> String {
    let test = test.to_owned();
    let test = test.replace(".ktest", "");
    let test = test + "." + subtest;
    let test = test.replace("/", ".");
    test
}

pub fn lockfile_exists(
    rc: &Ktestrc,
    commit: &str,
    test_name: &str,
    create: bool,
    commits_updated: &mut HashSet<String>,
) -> bool {
    let lockfile = rc.output_dir.join(commit).join(test_name).join("status");

    let timeout = std::time::Duration::from_secs(3600);
    let metadata = std::fs::metadata(&lockfile);

    if let Ok(metadata) = metadata {
        let elapsed = metadata
            .modified()
            .unwrap()
            .elapsed()
            .unwrap_or(std::time::Duration::from_secs(0));

        if metadata.is_file()
            && metadata.len() == 0
            && elapsed > timeout
            && std::fs::remove_file(&lockfile).is_ok()
        {
            eprintln!(
                "Deleted stale lock file {:?}, mtime {:?} now {:?} elapsed {:?})",
                &lockfile,
                metadata.modified().unwrap(),
                SystemTime::now(),
                elapsed
            );
            commits_updated.insert(commit.to_string());
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

#[derive(Debug)]
pub struct TestStats {
    pub nr: u64,
    pub passed: u64,
    pub failed: u64,
    pub duration: u64,
}

use durations_capnp::durations;
pub fn test_stats(durations: Option<&[u8]>, test: &str, subtest: &str) -> Option<TestStats> {
    if let Some(d) = durations {
        let mut d = d;

        let options = capnp::message::ReaderOptions {
            nesting_limit: 64,
            traversal_limit_in_words: Some(1024 * 1024 * 64), //  64 MiB limit
        };

        let d_reader = serialize::read_message_from_flat_slice(&mut d, options).ok();
        let d = d_reader
            .as_ref()
            .map(|x| x.get_root::<durations::Reader>().ok())
            .flatten();
        if d.is_none() {
            return None;
        }

        let d = d.unwrap().get_entries();
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
                eprintln!(
                    "error binary searching for test stats: error {:?} at idx {}/{}",
                    d_m_test,
                    m,
                    d.len()
                );
                return None;
            }

            let d_m_test = d_m_test.unwrap().to_str().unwrap();

            use std::cmp::Ordering::*;
            match full_test.cmp(d_m_test) {
                Less => r = m,
                Greater => l = m + 1,
                Equal => {
                    return Some(TestStats {
                        nr: d_m.get_nr(),
                        passed: d_m.get_passed(),
                        failed: d_m.get_failed(),
                        duration: d_m.get_duration(),
                    })
                }
            }
        }
    }

    None
}

// Shared query functions (used by both CGI and CLI)

use regex::Regex;

pub fn filter_results(r: TestResultsMap, tests_matching: &Regex) -> TestResultsMap {
    r.iter()
        .filter(|i| tests_matching.is_match(&i.0))
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

pub struct CommitResults {
    pub id: String,
    pub message: String,
    pub tests: TestResultsMap,
}

pub fn branch_get_results(
    repo: &git2::Repository,
    ktest: &Ktestrc,
    user: Option<&str>,
    branch: Option<&str>,
    commit: Option<&str>,
    tests_matching: &Regex,
) -> Result<Vec<CommitResults>, String> {
    let branch_or_commit = if let Some(commit) = commit {
        commit.to_string()
    } else {
        format!("{}/{}", user.unwrap(), branch.unwrap())
    };

    let mut walk = repo.revwalk().unwrap();

    let reference = git_get_commit(repo, branch_or_commit.clone());
    if reference.is_err() {
        return Err("commit not found".to_string());
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        return Err(format!("Error walking {}: {}", branch_or_commit, e));
    }

    // Phase 1: collect commit IDs from git (cheap, no I/O beyond git)
    let commits: Vec<(String, String)> = walk
        .filter_map(|i| i.ok())
        .filter_map(|i| repo.find_commit(i).ok())
        .take(150)
        .map(|c| (c.id().to_string(), c.message().unwrap_or("").to_string()))
        .collect();

    // Phase 2: prefetch all capnp files in parallel (HTTP/2 multiplexed)
    if let Some(ref base_url) = ktest.ci_url {
        let ids: Vec<String> = commits.iter().map(|(id, _)| id.clone()).collect();
        prefetch_capnp(base_url, &ktest.output_dir, &ids);
    }

    // Phase 3: build results from cache (now all filesystem reads)
    let mut nr_empty = 0;
    let mut nr_commits = 0;
    let mut ret: Vec<CommitResults> = Vec::new();

    for (id, message) in commits {
        let tests = commitdir_get_results(ktest, &id).unwrap_or(BTreeMap::new());
        let tests = filter_results(tests, tests_matching);

        let r = CommitResults { id, message, tests };

        if !r.tests.is_empty() {
            nr_empty = 0;
        } else {
            nr_empty += 1;
            if nr_empty > 100 {
                break;
            }
        }

        ret.push(r);

        nr_commits += 1;
        if nr_commits > 50 {
            break;
        }
    }

    while !ret.is_empty() && ret[ret.len() - 1].tests.is_empty() {
        ret.pop();
    }

    Ok(ret)
}

pub struct QueueStats {
    pub pending_by_user: BTreeMap<String, usize>,
    pub running_by_user: BTreeMap<String, usize>,
    pub total_pending: usize,
    pub total_running: usize,
}

pub fn get_queue_stats(config: &CiConfig) -> QueueStats {
    let mut stats = QueueStats {
        pending_by_user: BTreeMap::new(),
        running_by_user: BTreeMap::new(),
        total_pending: 0,
        total_running: 0,
    };

    for (user, _) in &config.users {
        let jobs_file = config.ktest.output_dir.join(format!("jobs.{}", user));
        let count = std::fs::read_to_string(&jobs_file)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0);
        if count > 0 {
            stats.pending_by_user.insert(user.clone(), count);
            stats.total_pending += count;
        }
    }

    if let Ok(workers) = workers_get(&config.ktest) {
        for w in workers {
            if !w.user.is_empty() && !w.tests.is_empty() {
                *stats.running_by_user.entry(w.user.clone()).or_insert(0) += 1;
                stats.total_running += 1;
            }
        }
    }

    stats
}

pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{:.1}h", secs as f64 / 3600.0)
    } else {
        format!("{:.1}d", secs as f64 / 86400.0)
    }
}

pub fn last_good_line(results: &[CommitResults], test: &str) -> String {
    for (idx, result) in results.iter().map(|i| i.tests.get(test)).enumerate() {
        if let Some(result) = result {
            if result.status == TestStatus::Passed {
                return format!("{}", idx);
            }

            if result.status != TestStatus::Failed {
                return format!(">= {}", idx);
            }
        } else {
            return format!(">= {}", idx);
        }
    }

    format!(">= {}", results.len())
}

// Branch log generation and parsing

use branchlog_capnp::branch_log;

pub struct BranchEntry {
    pub commit_id: String,
    pub message: String,
    pub passed: u32,
    pub failed: u32,
    pub notrun: u32,
    pub notstarted: u32,
    pub inprogress: u32,
    pub unknown: u32,
    pub duration: u64,
}

pub fn count_status(tests: &TestResultsMap, status: TestStatus) -> u32 {
    tests.iter().filter(|x| x.1.status == status).count() as u32
}

pub fn generate_branch_log(
    repo: &git2::Repository,
    ktest: &Ktestrc,
    user: &str,
    branch: &str,
) -> anyhow::Result<Vec<BranchEntry>> {
    let all = Regex::new("").unwrap();
    let results = branch_get_results(repo, ktest, Some(user), Some(branch), None, &all)
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(results
        .into_iter()
        .filter(|r| !r.tests.is_empty())
        .map(|r| {
            let duration: u64 = r.tests.iter().map(|x| x.1.duration).sum();
            BranchEntry {
                commit_id: r.id,
                message: r.message,
                passed: count_status(&r.tests, TestStatus::Passed),
                failed: count_status(&r.tests, TestStatus::Failed),
                notrun: count_status(&r.tests, TestStatus::Notrun),
                notstarted: count_status(&r.tests, TestStatus::Notstarted),
                inprogress: count_status(&r.tests, TestStatus::Inprogress),
                unknown: count_status(&r.tests, TestStatus::Unknown),
                duration,
            }
        })
        .collect())
}

pub fn write_branch_log(
    output_dir: &Path,
    user: &str,
    branch: &str,
    entries: &[BranchEntry],
) -> anyhow::Result<()> {
    let mut message = capnp::message::Builder::new_default();
    let log = message.init_root::<branch_log::Builder>();
    let mut list = log.init_entries(entries.len().try_into().unwrap());

    for (idx, entry) in entries.iter().enumerate() {
        let mut dst = list.reborrow().get(idx.try_into().unwrap());
        dst.set_commit_id(&entry.commit_id);
        dst.set_message(&entry.message);
        dst.set_passed(entry.passed);
        dst.set_failed(entry.failed);
        dst.set_notrun(entry.notrun);
        dst.set_notstarted(entry.notstarted);
        dst.set_inprogress(entry.inprogress);
        dst.set_unknown(entry.unknown);
        dst.set_duration(entry.duration);
    }

    let fname = output_dir.join(format!("branch.{}.{}.capnp", user, branch));
    let fname_new = output_dir.join(format!("branch.{}.{}.capnp.new", user, branch));

    let mut out = File::create(&fname_new).map(std::io::BufWriter::new)?;
    serialize::write_message(&mut out, &message)?;
    out.into_inner()?;
    std::fs::rename(fname_new, fname)?;

    Ok(())
}

pub fn branchlog_parse(f: &[u8]) -> anyhow::Result<Vec<BranchEntry>> {
    let message_reader =
        serialize::read_message_from_flat_slice(&mut &f[..], capnp::message::ReaderOptions::new())?;
    let entries = message_reader
        .get_root::<branch_log::Reader>()?
        .get_entries()?;

    let result = entries
        .iter()
        .map(|e| BranchEntry {
            commit_id: e.get_commit_id().unwrap().to_string().unwrap(),
            message: e.get_message().unwrap().to_string().unwrap(),
            passed: e.get_passed(),
            failed: e.get_failed(),
            notrun: e.get_notrun(),
            notstarted: e.get_notstarted(),
            inprogress: e.get_inprogress(),
            unknown: e.get_unknown(),
            duration: e.get_duration(),
        })
        .collect();

    Ok(result)
}

pub fn branchlog_get(ktest: &Ktestrc, user: &str, branch: &str) -> anyhow::Result<Vec<BranchEntry>> {
    let f = std::fs::read(ktest.output_dir.join(format!("branch.{}.{}.capnp", user, branch)))?;
    branchlog_parse(&f)
}
