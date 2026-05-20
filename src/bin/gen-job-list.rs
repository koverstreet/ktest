extern crate libc;
use anyhow;
use chrono::Utc;
use ci_cgi::TestResultsMap;
use ci_cgi::{
    ciconfig_read, commit_update_results_from_fs, commitdir_get_results, encode_env,
    git_get_commit, lockfile_exists, subtest_result_key, test_stats, users::RcBranch, CiConfig,
    RcTestGroup, Userrc,
};
use clap::Parser;
use file_lock::{FileLock, FileOptions};
use memmap::MmapOptions;
use memoize::memoize;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process;

#[memoize]
fn get_subtests(test_path: PathBuf) -> Vec<String> {
    let output = std::process::Command::new(&test_path)
        .arg("list-tests")
        .output()
        .expect(&format!("failed to execute process {:?} ", &test_path))
        .stdout;
    let output = String::from_utf8_lossy(&output);

    output.split_whitespace().map(|i| i.to_string()).collect()
}

#[derive(Debug)]
pub struct TestJob {
    user: String,
    repo: String,
    branch: String,
    commit: String,
    age: u64,
    nice: u64,
    duration: u64,
    /// Kernel-store identifier (e.g. "debian/forky"), or empty to mean
    /// "build the kernel from `repo` at `commit`" — the legacy
    /// build-test-kernel behavior.
    kernel: String,
    /// Encoded env to inject into the test invocation, "K1=V1,K2=V2"
    /// (empty string = no env). Keys/values must not contain space,
    /// `,`, or `=`.
    env: String,
    test: String,
    subtest: String,
}

fn testjob_weight(j: &TestJob) -> u64 {
    j.age + j.nice
}

use std::cmp::Ordering;

impl Ord for TestJob {
    fn cmp(&self, other: &Self) -> Ordering {
        testjob_weight(self)
            .cmp(&testjob_weight(other))
            .then(self.commit.cmp(&other.commit))
            .then(self.test.cmp(&other.test))
            .then(self.kernel.cmp(&other.kernel))
            .then(self.env.cmp(&other.env))
            .then(self.duration.cmp(&other.duration))
    }
}

impl PartialOrd for TestJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TestJob {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TestJob {}

fn have_result(results: &TestResultsMap, subtest: &str) -> bool {
    use ci_cgi::TestStatus;

    let r = results.get(subtest);
    if let Some(r) = r {
        let elapsed = Utc::now() - r.starttime;
        let timeout = chrono::Duration::minutes(30);

        r.status != TestStatus::Inprogress || elapsed < timeout
    } else {
        false
    }
}

fn branch_test_jobs(
    rc: &CiConfig,
    durations: Option<&[u8]>,
    user: &str,
    branch: &str,
    branch_repo: &str,
    branch_kernels: &[String],
    test_group: &RcTestGroup,
    test_path: &Path,
    verbose: bool,
) -> Vec<TestJob> {
    let test_name = &test_path.to_string_lossy();
    let full_test_path = rc.ktest.ktest_dir.join("tests").join(test_path);
    let mut ret = Vec::new();

    let encoded_env = encode_env(&test_group.env);
    if let Err(ref e) = encoded_env {
        eprintln!("test_group env unencodable: {}", e);
        return ret;
    }
    let encoded_env = encoded_env.unwrap();

    let path = match rc.ktest.repo_path(branch_repo) {
        Some(p) => p,
        None => {
            eprintln!("no path configured for repo {}", branch_repo);
            return ret;
        }
    };
    let repo = match git2::Repository::open(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error opening {:?}: {}", path, e);
            return ret;
        }
    };

    let subtests = get_subtests(full_test_path);

    if verbose {
        eprintln!(
            "looking for tests to run for branch {} test {:?} subtests {:?}",
            branch, test_path, subtests
        )
    }

    let userbranch = user.to_string() + "/" + branch;

    let mut walk = repo.revwalk().unwrap();
    let reference = git_get_commit(&repo, userbranch.clone());
    if reference.is_err() {
        eprintln!("branch {} not found", &userbranch);
        return ret;
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        eprintln!("Error walking {}: {}", &userbranch, e);
        return ret;
    }

    let mut commits_updated = HashSet::new();

    for (age, commit) in walk
        .filter_map(|i| i.ok())
        .filter_map(|i| repo.find_commit(i).ok())
        .take(test_group.max_commits as usize)
        .enumerate()
    {
        let commit = commit.id().to_string();

        let results = commitdir_get_results(&rc.ktest, &commit).unwrap_or(BTreeMap::new());

        if verbose {
            eprintln!("at commit {} age {}\nresults {:?}", &commit, age, results)
        }

        // Empty `branch_kernels` means "build the kernel from the
        // checked-out repo at the commit" — emit one job per subtest
        // with an empty kernel field. Otherwise fan out across the
        // configured kernel-store entries. The (commit, test, subtest,
        // kernel) tuple is the result-key, so kernels need separate
        // have-result / lockfile checks.
        let kernels: Vec<String> = if branch_kernels.is_empty() {
            vec![String::new()]
        } else {
            branch_kernels.to_vec()
        };

        for subtest in subtests.iter() {
            let mut nice = test_group.nice;

            let stats = test_stats(durations, test_name, &subtest);
            if let Some(ref stats) = stats {
                if test_group.test_always_passes_nice != 0
                    && !stats.passed != !stats.failed
                    && stats.passed + stats.failed > test_group.test_always_passes_nice
                {
                    nice += test_group.test_always_passes_nice;
                }

                if test_group.test_duration_nice != 0 {
                    nice += stats.duration / test_group.test_duration_nice;
                }
            }

            let duration = stats.map_or(rc.ktest.subtest_duration_def.unwrap_or(30), |s| s.duration);

            for kernel in &kernels {
                let key = subtest_result_key(&test_name, subtest, kernel);
                if have_result(&results, &key) {
                    continue;
                }
                if lockfile_exists(&rc.ktest, &commit, &key, false, &mut commits_updated) {
                    continue;
                }

                ret.push(TestJob {
                    user: user.to_string(),
                    repo: branch_repo.to_string(),
                    branch: branch.to_string(),
                    commit: commit.clone(),
                    age: age as u64,
                    nice,
                    duration,
                    kernel: kernel.clone(),
                    env: encoded_env.clone(),
                    test: test_name.to_string(),
                    subtest: subtest.clone(),
                });
            }
        }
    }

    for i in commits_updated.iter() {
        commit_update_results_from_fs(&rc.ktest, &i);
    }

    ret
}

fn user_test_jobs(
    rc: &CiConfig,
    durations: Option<&[u8]>,
    user: &str,
    userconfig: &Userrc,
    verbose: bool,
) -> Vec<TestJob> {
    let mut ret: Vec<_> = userconfig
        .branches
        .iter()
        .flat_map(move |(branch, branchconfig)| {
            branchconfig
                .test_groups
                .iter()
                .filter_map(|i| userconfig.test_groups.get(i))
                .map(move |testgroup| (branch, branchconfig, testgroup))
        })
        .flat_map(move |(branch, branchconfig, testgroup)| {
            testgroup.tests.iter().flat_map(move |test| {
                branch_test_jobs(
                    rc, durations, user, &branch,
                    &branchconfig.repo, &testgroup.kernels,
                    &testgroup, &test, verbose,
                )
            })
        })
        .collect();

    /* sort by commit, dedup */

    ret.sort();
    ret.reverse();
    ret
}

fn rc_test_jobs(
    rc: &CiConfig,
    durations: Option<&[u8]>,
    verbose: bool,
) -> Vec<TestJob> {
    let mut ret: Vec<TestJob> = Vec::new();

    let users: Vec<_> = rc
        .users
        .iter()
        .filter(|u| u.1.is_ok())
        .map(|(user, userconfig)| (user, userconfig.as_ref().unwrap()))
        .collect();

    eprintln!("Generating jobs for {} users...", users.len());

    for (user, userconfig) in users {
        let jobs = user_test_jobs(rc, durations, &user, &userconfig, verbose);
        if !jobs.is_empty() {
            eprintln!("  {}: {} jobs", user, jobs.len());
        }
        ret.extend(jobs);
    }

    /* sort by commit, dedup */

    ret.sort();
    ret.reverse();
    ret
}

fn write_test_jobs(rc: &CiConfig, jobs_in: Vec<TestJob>, verbose: bool) -> anyhow::Result<()> {
    eprintln!("Writing {} test jobs...", jobs_in.len());

    if verbose {
        eprint!("jobs: {:?}", jobs_in);
    }

    // Group jobs by user
    let mut jobs_by_user: BTreeMap<String, Vec<&TestJob>> = BTreeMap::new();
    for job in jobs_in.iter() {
        jobs_by_user
            .entry(job.user.clone())
            .or_insert_with(Vec::new)
            .push(job);
    }

    // Write per-user job files
    for (user, jobs) in jobs_by_user.iter() {
        let jobs_fname = rc.ktest.output_dir.join(format!("jobs.{}", user));
        let jobs_fname_new = rc.ktest.output_dir.join(format!("jobs.{}.new", user));
        let mut jobs_out = std::io::BufWriter::new(File::create(&jobs_fname_new)?);

        for job in jobs.iter() {
            // Format: <repo> <branch> <commit> <age> <kernel-or-`-`> <env-or-`-`> <test> <subtest>
            // `-` is the sentinel for empty (no kernel = build from repo;
            // no env = no overrides). Kernel-store identifiers always
            // contain `/` and env is encoded `K=V,K=V`, so neither can
            // collide with the `-` sentinel.
            let kernel = if job.kernel.is_empty() { "-" } else { job.kernel.as_str() };
            let env = if job.env.is_empty() { "-" } else { job.env.as_str() };
            jobs_out.write(job.repo.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.branch.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.commit.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.age.to_string().as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(kernel.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(env.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.test.as_bytes())?;
            jobs_out.write(b" ")?;
            jobs_out.write(job.subtest.as_bytes())?;
            jobs_out.write(b"\n")?;
        }

        jobs_out.flush()?;
        drop(jobs_out);
        std::fs::rename(&jobs_fname_new, &jobs_fname)?;
        eprintln!("  {} jobs for user {}", jobs.len(), user);
    }

    // Clean up job files for users with no jobs
    if let Ok(entries) = std::fs::read_dir(&rc.ktest.output_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("jobs.")
                && !name_str.ends_with(".new")
                && !name_str.ends_with(".lock")
            {
                let user = name_str.strip_prefix("jobs.").unwrap();
                if !jobs_by_user.contains_key(user) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    Ok(())
}

fn fetch_remotes(rc: &CiConfig) -> anyhow::Result<bool> {
    fn fetch_branch(
        rc: &CiConfig,
        user: &str,
        branch: &str,
        branchconfig: &RcBranch,
    ) -> anyhow::Result<()> {
        let path = match rc.ktest.repo_path(&branchconfig.repo) {
            Some(p) => p,
            None => {
                eprintln!("no path configured for repo {}", branchconfig.repo);
                return Ok(());
            }
        };
        let fetch = branchconfig.fetch.split_whitespace().map(|i| OsStr::new(i));

        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("fetch")
            .args(fetch)
            .status()
            .expect(&format!("failed to execute fetch"));
        if !status.success() {
            eprintln!("fetch error for {} in {:?}: {}", branchconfig.fetch, path, status);
            return Ok(());
        }

        let repo = git2::Repository::open(path)
            .map_err(|e| {
                eprintln!("error opening {:?}: {}", path, e);
                e
            })?;

        let fetch_head = repo
            .revparse_single("FETCH_HEAD")
            .map_err(|e| {
                eprintln!("error parsing FETCH_HEAD in {:?}: {}", path, e);
                e
            })?
            .peel_to_commit()
            .map_err(|e| {
                eprintln!("error getting FETCH_HEAD in {:?}: {}", path, e);
                e
            })?;

        repo.branch(&(user.to_string() + "/" + branch), &fetch_head, true)?;
        Ok(())
    }

    let lockfile = rc.ktest.output_dir.join("fetch.lock");
    let metadata = std::fs::metadata(&lockfile);
    if let Ok(metadata) = metadata {
        let elapsed = metadata.modified().unwrap().elapsed().unwrap_or_default();

        if elapsed < std::time::Duration::from_secs(30) {
            return Ok(false);
        }
    }

    let mut filelock =
        FileLock::lock(lockfile, false, FileOptions::new().create(true).write(true))?;

    eprint!("Fetching remotes...");
    for (user, userconfig) in rc
        .users
        .iter()
        .filter(|u| u.1.is_ok())
        .map(|(user, userconfig)| (user, userconfig.as_ref().unwrap()))
    {
        for (branch, branchconfig) in &userconfig.branches {
            fetch_branch(rc, user, branch, branchconfig).ok();
        }
    }
    eprintln!(" done");

    filelock.file.write_all(b"ok")?; /* update lockfile mtime */

    /*
     * XXX: return true only if remotes actually changed
     */
    Ok(true)
}

fn update_jobs(rc: &CiConfig, args: &Args) -> anyhow::Result<()> {
    if fetch_remotes(rc).ok() == Some(false) && !args.force_update_jobs {
        eprintln!("remotes unchanged, skipping updating joblist");
        return Ok(());
    }

    let durations_file = File::open(rc.ktest.output_dir.join("test_durations.capnp")).ok();
    let durations_map = durations_file
        .map(|x| unsafe { MmapOptions::new().map(&x).ok() })
        .flatten();
    let durations = durations_map.as_ref().map(|x| x.as_ref());

    let lockfile = rc.ktest.output_dir.join("jobs.lock");
    let filelock = FileLock::lock(lockfile, true, FileOptions::new().create(true).write(true))?;

    let jobs_in = rc_test_jobs(rc, durations, false);
    write_test_jobs(rc, jobs_in, args.verbose)?;

    drop(filelock);

    Ok(())
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    verbose: bool,
    #[arg(long)]
    force_update_jobs: bool,
    /// Parse the given user config file and print the test-job matrix
    /// it would generate, without consulting git or existing results.
    /// Useful for validating a config + previewing what'll be queued.
    #[arg(long)]
    check: Option<PathBuf>,
}

fn check_config(rc: &CiConfig, path: &Path) -> anyhow::Result<()> {
    let userrc = ci_cgi::users::userrc_read(path)?;

    println!("test_groups ({}):", userrc.test_groups.len());
    for (name, g) in &userrc.test_groups {
        let env_s = encode_env(&g.env)?;
        println!(
            "  {}: max_commits={} nice={} kernels={:?} env={} tests={}",
            name,
            g.max_commits,
            g.nice,
            g.kernels,
            if env_s.is_empty() { "-".to_string() } else { env_s },
            g.tests.len(),
        );
    }

    println!();
    println!("expansion (branch test_group kernel env test subtest):");

    let mut total = 0u64;
    for (branch_name, branch) in &userrc.branches {
        for tg_name in &branch.test_groups {
            let tg = userrc
                .test_groups
                .get(tg_name)
                .expect("validated at parse time");
            let env = encode_env(&tg.env)?;
            let env_field = if env.is_empty() { "-".to_string() } else { env };
            let kernels: Vec<String> = if tg.kernels.is_empty() {
                vec!["-".to_string()]
            } else {
                tg.kernels.clone()
            };
            for test_path in &tg.tests {
                let full = rc.ktest.ktest_dir.join("tests").join(test_path);
                let subtests = get_subtests(full);
                if subtests.is_empty() {
                    eprintln!(
                        "warning: no subtests for {} (check the file exists + lists tests)",
                        test_path.display()
                    );
                }
                for kernel in &kernels {
                    for subtest in &subtests {
                        println!(
                            "{} {} {} {} {} {}",
                            branch_name,
                            tg_name,
                            kernel,
                            env_field,
                            test_path.display(),
                            subtest
                        );
                        total += 1;
                    }
                }
            }
        }
    }
    eprintln!("{} jobs per commit", total);
    Ok(())
}

fn main() {
    let args = Args::parse();

    let rc = ciconfig_read();
    if let Err(e) = rc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let rc = rc.unwrap();

    if let Some(ref path) = args.check {
        if let Err(e) = check_config(&rc, path) {
            eprintln!("check error: {:#}", e);
            process::exit(1);
        }
        return;
    }

    // Report users with broken configs
    let broken_users: Vec<_> = rc
        .users
        .iter()
        .filter_map(|(user, result)| result.as_ref().err().map(|e| (user, e)))
        .collect();
    if !broken_users.is_empty() {
        eprintln!("Warning: {} users have config errors:", broken_users.len());
        for (user, err) in broken_users {
            eprintln!("  {}: {}", user, err);
        }
    }

    if let Err(e) = update_jobs(&rc, &args) {
        eprintln!("update_jobs() error: {}", e);
    }
}
