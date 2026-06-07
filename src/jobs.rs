// Job-list matrix: the test jobs the CI config wants run — the matrix
// (commits × test_groups × tests × subtests × kernels) minus jobs that
// already have a result on disk.
//
// Ported from gen-job-list for the push-mode daemon. desired_jobs()
// emits commit-age-major (every config's age-0 / branch-tip jobs, then
// age-1, …) and stops at a caller-given limit — so the daemon can pull
// a bounded window of the *newest* commits' work without materializing
// the whole (potentially millions-of-jobs) matrix.
//
// Pure read of (config, git refs, results) — it does not fetch git or
// refresh result caches; the daemon owns those.

use crate::{
    encode_env, git_get_commit, subtest_result_key, test_stats, CiConfig, RcTestGroup,
    TestResultsMap, TestResultsStore, TestStats, TestStatus,
};
use memmap::MmapOptions;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

/// Identity of a test job — the tuple that uniquely names a unit of
/// test work. The daemon's job map diffs the desired set on this key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobKey {
    pub user: String,
    pub repo: String,
    pub branch: String,
    pub commit: String,
    /// Kernel-store id (e.g. "debian/forky"); empty = build from repo.
    pub kernel: String,
    /// Encoded env ("K1=V1,K2=V2"); empty = none.
    pub env: String,
    /// Test path under tests/, e.g. "fs/bcachefs/ec.ktest".
    pub test: String,
    pub subtest: String,
}

/// A desired test job: its identity plus scheduling hints.
#[derive(Debug, Clone)]
pub struct Job {
    pub key: JobKey,
    /// Commits behind the branch tip (0 = tip). Lower = run sooner.
    pub age: u64,
    /// Niceness — higher = lower scheduling priority.
    pub nice: i64,
    /// Expected runtime in seconds, from historical durations.
    pub duration: u64,
}

/// List the subtests of a .ktest file. Cached — the same test shows up
/// across many branches/groups, and listing spawns the file. Only a
/// *successful* listing is cached: a transient list-tests failure (a
/// spawn error or a non-zero exit) must not stick a whole test out of
/// the job matrix for the daemon's entire lifetime — the next call
/// retries instead.
fn get_subtests(test_path: PathBuf) -> Vec<String> {
    static CACHE: LazyLock<Mutex<HashMap<PathBuf, Vec<String>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    if let Some(hit) = CACHE.lock().unwrap().get(&test_path) {
        return hit.clone();
    }

    let subtests = match std::process::Command::new(&test_path)
        .arg("list-tests")
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .map(|s| s.to_string())
            .collect(),
        Ok(o) => {
            eprintln!("listing subtests of {:?}: list-tests exited {:?}",
                      test_path, o.status.code());
            Vec::new()
        }
        Err(e) => {
            eprintln!("listing subtests of {:?}: {}", test_path, e);
            Vec::new()
        }
    };

    // Cache successes only; an empty result — a failure, or a genuinely
    // subtest-less test — re-lists on the next call rather than sticking.
    if !subtests.is_empty() {
        CACHE.lock().unwrap().insert(test_path, subtests.clone());
    }
    subtests
}

/// True once a *verdict* was recorded — the job is done, re-running
/// won't change it. Verdicts:
///   - Passed / Failed — the test ran and reported (a kernel panic
///                  while it runs counts as Failed).
///   - Notrun     — a test reporting it deliberately did not run.
///   - FailedToRun — the daemon never managed to launch this subtest in
///                  up, or the supervisor couldn't launch it. A
///                  CI-side failure; terminal, surfaced not re-run.
/// Not verdicts:
///   - Unknown    — a garbled or partially-written status file; re-run.
///   - Inprogress — a job in flight: not a verdict, but desired_jobs()
///                  must not re-emit it either — that would double-run
///                  the VM (see job_wanted()). Stale Inprogress markers
///                  from a crashed daemon are deleted at startup.
///
/// Whitelist, not blacklist: a future TestStatus variant defaults to
/// "not done" — at worst a wasted re-run, never a silent loss.
fn result_is_done(status: TestStatus) -> bool {
    matches!(status,
             TestStatus::Passed | TestStatus::Failed |
             TestStatus::Notrun | TestStatus::FailedToRun)
}

/// Whether desired_jobs() should emit a job for a subtest, given its
/// recorded result status (None = no result yet). Emit when there is no
/// result, or a non-verdict, non-running one (Unknown); skip a verdict,
/// and skip Inprogress — that job is in flight, and re-emitting it would
/// double-run the VM.
fn job_wanted(status: Option<TestStatus>) -> bool {
    match status {
        None => true,
        Some(s) => !result_is_done(s) && s != TestStatus::Inprogress,
    }
}

/// Niceness for one subtest: the test_group's base nice plus the
/// historical-stats adjustments — nice down tests that consistently
/// pass-or-fail, and long-running tests.
fn job_nice(tg: &RcTestGroup, stats: Option<&TestStats>) -> i64 {
    let mut nice = tg.nice as i64;
    if let Some(stats) = stats {
        if tg.test_always_passes_nice != 0
            && stats.passed != stats.failed
            && stats.passed + stats.failed > tg.test_always_passes_nice
        {
            nice += tg.test_always_passes_nice as i64;
        }
        if tg.test_duration_nice != 0 {
            nice += (stats.duration / tg.test_duration_nice) as i64;
        }
    }
    nice
}

/// One (user, branch, test_group, test) and its branch's commit list —
/// enough to emit that test's jobs commit-by-commit.
struct TestSpec<'a> {
    user: String,
    repo: String,
    branch: String,
    test: String,
    env: String,
    tg: &'a RcTestGroup,
    subtests: Vec<String>,
    kernels: Vec<String>,
    /// Commit ids newest-first, capped at tg.max_commits.
    commits: Vec<String>,
}

/// The branch's commit ids, newest-first, capped at `max`.
fn branch_commits(git: &git2::Repository, userbranch: &str, max: usize) -> Option<Vec<String>> {
    let reference = git_get_commit(git, userbranch.to_string())
        .map_err(|_| eprintln!("branch {} not found", userbranch))
        .ok()?;
    let mut walk = git
        .revwalk()
        .map_err(|e| eprintln!("revwalk error for {}: {}", userbranch, e))
        .ok()?;
    walk.push(reference.id())
        .map_err(|e| eprintln!("error walking {}: {}", userbranch, e))
        .ok()?;
    Some(
        walk.filter_map(|i| i.ok())
            .take(max)
            .map(|id| id.to_string())
            .collect(),
    )
}

/// Build one TestSpec per (user, branch, test_group, test) across the
/// whole CI config, each carrying its branch's commit list.
fn build_test_specs<'a>(rc: &'a CiConfig) -> Vec<TestSpec<'a>> {
    let mut specs = Vec::new();
    for (user, userconfig) in &rc.users {
        let userconfig = match userconfig {
            Ok(u) => u,
            Err(_) => continue, // broken config — reported elsewhere
        };
        for (branch, branchconfig) in &userconfig.branches {
            for tg_name in &branchconfig.test_groups {
                let tg = match userconfig.test_groups.get(tg_name) {
                    Some(tg) => tg,
                    None => continue, // undefined group — validated at parse time
                };
                let env = match encode_env(&tg.env) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("test_group env for branch {} unencodable: {}", branch, e);
                        continue;
                    }
                };
                let repo_path = match rc.ktest.repo_path(&branchconfig.repo) {
                    Some(p) => p,
                    None => {
                        eprintln!("no path configured for repo {}", branchconfig.repo);
                        continue;
                    }
                };
                let git = match git2::Repository::open(repo_path) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("error opening {:?}: {}", repo_path, e);
                        continue;
                    }
                };
                let userbranch = format!("{}/{}", user, branch);
                let commits = match branch_commits(&git, &userbranch, tg.max_commits as usize) {
                    Some(c) => c,
                    None => continue,
                };
                let kernels = if tg.kernels.is_empty() {
                    vec![String::new()]
                } else {
                    tg.kernels.clone()
                };
                for test in &tg.tests {
                    let test = test.to_string_lossy().to_string();
                    let subtests = get_subtests(rc.ktest.ktest_dir.join("tests").join(&test));
                    if subtests.is_empty() {
                        continue;
                    }
                    specs.push(TestSpec {
                        user: user.clone(),
                        repo: branchconfig.repo.clone(),
                        branch: branch.clone(),
                        test,
                        env: env.clone(),
                        tg,
                        subtests,
                        kernels: kernels.clone(),
                        commits: commits.clone(),
                    });
                }
            }
        }
    }
    specs
}

/// Per-job scheduling weight — lower runs sooner.
///
/// Faithful port of gen-job-list's testjob_weight: age + nice. A
/// low-nice branch's older commits outrank a high-nice branch's tip;
/// within a (branch, nice) class, newer commits naturally come first.
fn job_weight(j: &Job) -> i64 {
    j.age as i64 + j.nice
}

/// The desired test jobs, priority-ordered (lowest weight first),
/// capped at `limit`.
///
/// Emits the full candidate matrix, sorts by `(age + nice)` weight with
/// commit/test/kernel/env/duration as tiebreakers (matching the old
/// gen-job-list ordering), then truncates. Pure read; does not fetch
/// git.
///
/// May contain duplicate `JobKey`s if the config routes the same
/// (test, kernel, env) through two test_groups on one branch; the
/// daemon's job map collapses those.
pub fn desired_jobs(rc: &CiConfig, results: &TestResultsStore, limit: usize) -> Vec<Job> {
    // Historical per-subtest durations, for the nice/duration hints.
    let durations_map = std::fs::File::open(rc.ktest.output_dir.join("test_durations.capnp"))
        .ok()
        .and_then(|f| unsafe { MmapOptions::new().map(&f).ok() });
    let durations = durations_map.as_ref().map(|m| m.as_ref());

    let specs = build_test_specs(rc);
    let max_age = specs.iter().map(|s| s.commits.len()).max().unwrap_or(0);

    // A commit's results are fetched once (from the in-mem store) and
    // shared across the specs that touch that commit at the same age.
    let mut results_cache: HashMap<String, TestResultsMap> = HashMap::new();
    let mut out = Vec::new();

    for age in 0..max_age {
        for spec in &specs {
            let commit = match spec.commits.get(age) {
                Some(c) => c,
                None => continue,
            };
            let results = results_cache
                .entry(commit.clone())
                .or_insert_with(|| results.commit_results(commit).unwrap_or_default());
            for subtest in &spec.subtests {
                for kernel in &spec.kernels {
                    // stats are keyed per (subtest, kernel, env) — same key
                    // the durations capnp uses, so the lookup must be in here
                    // where kernel/env are known, not hoisted out.
                    let stats = test_stats(durations, &spec.test, subtest, kernel, &spec.env);
                    let nice = job_nice(spec.tg, stats.as_ref());
                    let duration = stats
                        .map(|s| s.duration)
                        .unwrap_or(rc.ktest.subtest_duration_def.unwrap_or(30));
                    let key = subtest_result_key(&spec.test, subtest, kernel, &spec.env);
                    if !job_wanted(results.get(&key).map(|r| r.status)) {
                        continue;
                    }
                    out.push(Job {
                        key: JobKey {
                            user: spec.user.clone(),
                            repo: spec.repo.clone(),
                            branch: spec.branch.clone(),
                            commit: commit.clone(),
                            kernel: kernel.clone(),
                            env: spec.env.clone(),
                            test: spec.test.clone(),
                            subtest: subtest.clone(),
                        },
                        age: age as u64,
                        nice,
                        duration,
                    });
                }
            }
        }
    }

    out.sort_by(|a, b| {
        job_weight(a)
            .cmp(&job_weight(b))
            .then(a.key.commit.cmp(&b.key.commit))
            .then(a.key.test.cmp(&b.key.test))
            .then(a.key.kernel.cmp(&b.key.kernel))
            .then(a.key.env.cmp(&b.key.env))
            .then(a.duration.cmp(&b.duration))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_needs_a_verdict() {
        // verdicts — re-running won't change them
        assert!(result_is_done(TestStatus::Passed));
        assert!(result_is_done(TestStatus::Failed));
        assert!(result_is_done(TestStatus::Notrun));
        assert!(result_is_done(TestStatus::FailedToRun)); // daemon failed to run — terminal
        // not verdicts — must re-run, not silently "done"
        assert!(!result_is_done(TestStatus::Inprogress)); // still running
        assert!(!result_is_done(TestStatus::Unknown));    // garbled status
    }

    #[test]
    fn inprogress_is_not_re_emitted() {
        assert!(job_wanted(None));                          // never run
        assert!(job_wanted(Some(TestStatus::Unknown)));     // garbled — re-run
        assert!(!job_wanted(Some(TestStatus::Passed)));     // verdict
        assert!(!job_wanted(Some(TestStatus::Inprogress))); // in flight — don't double-run
    }
}
