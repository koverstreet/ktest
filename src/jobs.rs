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
    commitdir_get_results, encode_env, git_get_commit, subtest_result_key, test_stats, CiConfig,
    RcTestGroup, TestStats, TestStatus,
};
use memmap::MmapOptions;
use memoize::memoize;
use std::collections::HashMap;
use std::path::PathBuf;

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
    pub nice: u64,
    /// Expected runtime in seconds, from historical durations.
    pub duration: u64,
}

/// List the subtests of a .ktest file. Memoized — the same test shows
/// up across many branches/groups, and listing spawns the file.
#[memoize]
fn get_subtests(test_path: PathBuf) -> Vec<String> {
    match std::process::Command::new(&test_path)
        .arg("list-tests")
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .map(|s| s.to_string())
            .collect(),
        Err(e) => {
            eprintln!("failed to list subtests of {:?}: {}", test_path, e);
            Vec::new()
        }
    }
}

/// True once a *verdict* was recorded — the job is done, re-running
/// won't change it. Only Passed, Failed, and Notrun (a test reporting
/// it deliberately did not run) are verdicts. The rest are not, and
/// must re-run:
///   - Inprogress  — still running (or a stale marker from a daemon
///                   restart; the in-memory table tracks the real set)
///   - Unknown     — a garbled or partially-written status file
///   - Notstarted  — a CI failure to *start* the test: an error, not
///                   a result
///
/// Whitelist, not blacklist: a future TestStatus variant defaults to
/// "not done" — at worst a wasted re-run, never a silent loss.
fn result_is_done(status: TestStatus) -> bool {
    matches!(status,
             TestStatus::Passed | TestStatus::Failed | TestStatus::Notrun)
}

/// Niceness for one subtest: the test_group's base nice plus the
/// historical-stats adjustments — nice down tests that consistently
/// pass-or-fail, and long-running tests.
fn job_nice(tg: &RcTestGroup, stats: Option<&TestStats>) -> u64 {
    let mut nice = tg.nice;
    if let Some(stats) = stats {
        if tg.test_always_passes_nice != 0
            && stats.passed != stats.failed
            && stats.passed + stats.failed > tg.test_always_passes_nice
        {
            nice += tg.test_always_passes_nice;
        }
        if tg.test_duration_nice != 0 {
            nice += stats.duration / tg.test_duration_nice;
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

/// The desired test jobs, newest-commit-first, capped at `limit`.
///
/// Emits commit-age-major: every config's age-0 (branch-tip) jobs, then
/// age-1, and so on — so a bounded prefix is exactly the newest
/// commits' work, and the daemon can window without materializing the
/// whole matrix. Pure read; does not fetch git.
///
/// May contain duplicate `JobKey`s if the config routes the same
/// (test, kernel, env) through two test_groups on one branch; the
/// daemon's job map collapses those.
pub fn desired_jobs(rc: &CiConfig, limit: usize) -> Vec<Job> {
    // Historical per-subtest durations, for the nice/duration hints.
    let durations_map = std::fs::File::open(rc.ktest.output_dir.join("test_durations.capnp"))
        .ok()
        .and_then(|f| unsafe { MmapOptions::new().map(&f).ok() });
    let durations = durations_map.as_ref().map(|m| m.as_ref());

    let specs = build_test_specs(rc);
    let max_age = specs.iter().map(|s| s.commits.len()).max().unwrap_or(0);

    // A commit's results are fetched once and shared across the specs
    // that touch that commit at the same age.
    let mut results_cache: HashMap<String, _> = HashMap::new();
    let mut out = Vec::new();

    for age in 0..max_age {
        for spec in &specs {
            let commit = match spec.commits.get(age) {
                Some(c) => c,
                None => continue,
            };
            let results = results_cache
                .entry(commit.clone())
                .or_insert_with(|| commitdir_get_results(&rc.ktest, commit).unwrap_or_default());
            for subtest in &spec.subtests {
                let stats = test_stats(durations, &spec.test, subtest);
                let nice = job_nice(spec.tg, stats.as_ref());
                let duration = stats
                    .map(|s| s.duration)
                    .unwrap_or(rc.ktest.subtest_duration_def.unwrap_or(30));
                for kernel in &spec.kernels {
                    let key = subtest_result_key(&spec.test, subtest, kernel, &spec.env);
                    if results.get(&key).map(|r| result_is_done(r.status)) == Some(true) {
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
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
    }
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
        // not verdicts — must re-run, not silently "done"
        assert!(!result_is_done(TestStatus::Inprogress)); // still running
        assert!(!result_is_done(TestStatus::Unknown));    // garbled status
        assert!(!result_is_done(TestStatus::Notstarted)); // CI failed to start it
    }
}
