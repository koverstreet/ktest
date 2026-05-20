// Job-list matrix: the set of test jobs *desired* by the CI config —
// the full matrix (commits × test_groups × tests × subtests × kernels)
// minus jobs that already have a result on disk.
//
// This is the port of gen-job-list's matrix computation for the
// push-mode daemon. It deliberately omits the lockfile in-flight check:
// in push mode "what's running" is the daemon's in-memory job table,
// and reconcile diffs desired_jobs() against that table directly.
//
// desired_jobs() does not fetch git or refresh result caches — the
// daemon owns those. It is a pure read of (config, git refs, results).

use crate::{
    commitdir_get_results, encode_env, git_get_commit, subtest_result_key, test_stats, CiConfig,
    Ktestrc, RcTestGroup, TestStats, TestStatus,
};
use memmap::MmapOptions;
use memoize::memoize;
use std::path::PathBuf;

/// Identity of a test job — the tuple that uniquely names a unit of
/// test work. Reconcile diffs the desired set against the running set
/// on this key.
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

/// True once a result exists and is not Inprogress: the job is done and
/// re-running won't change the verdict — Notstarted/Unknown count, a
/// result entry exists. Inprogress is *not* done; in push mode the
/// daemon's in-memory table tracks what is actually running, so a stale
/// Inprogress (e.g. left by a daemon restart) correctly re-runs.
fn result_is_done(status: TestStatus) -> bool {
    status != TestStatus::Inprogress
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

/// Emit the desired jobs for one (branch, test_group, test): walk the
/// branch's commits, fan out over subtests × kernels, skip any already
/// done.
fn branch_group_test_jobs(
    rc: &Ktestrc,
    durations: Option<&[u8]>,
    user: &str,
    branch: &str,
    repo: &str,
    tg: &RcTestGroup,
    test: &str,
    out: &mut Vec<Job>,
) {
    let env = match encode_env(&tg.env) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("test_group env for branch {} unencodable: {}", branch, e);
            return;
        }
    };

    let repo_path = match rc.repo_path(repo) {
        Some(p) => p,
        None => {
            eprintln!("no path configured for repo {}", repo);
            return;
        }
    };
    let git = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error opening {:?}: {}", repo_path, e);
            return;
        }
    };

    let subtests = get_subtests(rc.ktest_dir.join("tests").join(test));
    if subtests.is_empty() {
        return;
    }

    let userbranch = format!("{}/{}", user, branch);
    let reference = match git_get_commit(&git, userbranch.clone()) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("branch {} not found", userbranch);
            return;
        }
    };
    let mut walk = match git.revwalk() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("revwalk error for {}: {}", userbranch, e);
            return;
        }
    };
    if let Err(e) = walk.push(reference.id()) {
        eprintln!("error walking {}: {}", userbranch, e);
        return;
    }

    // Empty kernel list = build the kernel from the commit's own repo.
    let kernels: Vec<String> = if tg.kernels.is_empty() {
        vec![String::new()]
    } else {
        tg.kernels.clone()
    };

    for (age, commit) in walk
        .filter_map(|i| i.ok())
        .filter_map(|i| git.find_commit(i).ok())
        .take(tg.max_commits as usize)
        .enumerate()
    {
        let commit = commit.id().to_string();
        let results = commitdir_get_results(rc, &commit).unwrap_or_default();

        for subtest in &subtests {
            let stats = test_stats(durations, test, subtest);
            let nice = job_nice(tg, stats.as_ref());
            let duration = stats
                .map(|s| s.duration)
                .unwrap_or(rc.subtest_duration_def.unwrap_or(30));

            for kernel in &kernels {
                let key = subtest_result_key(test, subtest, kernel, &env);
                if results.get(&key).map(|r| result_is_done(r.status)) == Some(true) {
                    continue;
                }
                out.push(Job {
                    key: JobKey {
                        user: user.to_string(),
                        repo: repo.to_string(),
                        branch: branch.to_string(),
                        commit: commit.clone(),
                        kernel: kernel.clone(),
                        env: env.clone(),
                        test: test.to_string(),
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

/// The full set of test jobs the CI config currently wants run: the
/// matrix minus jobs already done. Pure read — does not fetch git or
/// touch result caches.
///
/// May contain duplicate `JobKey`s if the config routes the same
/// (test, kernel, env) through two test_groups on one branch; the
/// daemon's reconcile map collapses those.
pub fn desired_jobs(rc: &CiConfig) -> Vec<Job> {
    // Historical per-subtest durations, for the nice/duration hints.
    let durations_map = std::fs::File::open(rc.ktest.output_dir.join("test_durations.capnp"))
        .ok()
        .and_then(|f| unsafe { MmapOptions::new().map(&f).ok() });
    let durations = durations_map.as_ref().map(|m| m.as_ref());

    let mut out = Vec::new();
    for (user, userconfig) in &rc.users {
        let userconfig = match userconfig {
            Ok(u) => u,
            Err(_) => continue, // broken config — reported elsewhere
        };
        for (branch, branchconfig) in &userconfig.branches {
            for tg_name in &branchconfig.test_groups {
                let tg = match userconfig.test_groups.get(tg_name) {
                    Some(tg) => tg,
                    None => continue, // undefined test_group — validated at parse time
                };
                for test in &tg.tests {
                    branch_group_test_jobs(
                        &rc.ktest,
                        durations,
                        user,
                        branch,
                        &branchconfig.repo,
                        tg,
                        &test.to_string_lossy(),
                        &mut out,
                    );
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
    fn done_excludes_only_inprogress() {
        assert!(result_is_done(TestStatus::Passed));
        assert!(result_is_done(TestStatus::Failed));
        assert!(result_is_done(TestStatus::Notrun));
        assert!(result_is_done(TestStatus::Notstarted));
        assert!(result_is_done(TestStatus::Unknown));
        assert!(!result_is_done(TestStatus::Inprogress));
    }
}
