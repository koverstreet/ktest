// CI worker: poll a jobserver for test jobs, run them, ship results back.
//
// Replaces ci/test-git-branch.sh. Same CLI surface:
//
//     test-git-branch [-o] [-v] JOBSERVER
//
// Differences from the old script:
//
//  * State-dir model. Cwd is the worker's state dir, not a kernel clone.
//    The repo named in the job is cloned into ./<repo>/ on demand.
//    Same-repo consecutive jobs keep the checkout (just fetch +
//    checkout the new commit) and the kernel build cache; switching
//    repos wipes the state dir, since the per-arch kernel build
//    artifacts are keyed to a specific source tree.
//
//  * $ktest_deps_dir is exported so require-git places test
//    dependencies in the state dir instead of polluting the ktest
//    source tree. The dirname require-git computes from a clone URL
//    matches the repo short name, so the worker's checkout naturally
//    satisfies require-git for the repo-under-test.
//
//  * Drops JOBSERVER_GIT_REPOS / sync_git_repos pre-fetch. Repos are
//    fetched lazily when a job for them arrives.
//
//  * No tput cursor-erase tricks: workers run under ssh, output goes
//    to the head node's log. Append-only is easier to read.

use anyhow::{anyhow, bail, Context, Result};
use chrono::Local;
use clap::Parser;
use rand::Rng;
use std::env;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::Duration;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(author, version, about = "CI worker: poll a jobserver for test jobs")]
struct Args {
    /// Run a single job and exit.
    #[arg(short = 'o', long)]
    once: bool,

    /// Verbose logging.
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Jobserver hostname.
    jobserver: String,
}

/// Subset of the jobserver's .ktestrc that the worker uses.
#[derive(Debug, Default)]
struct JobserverRc {
    jobserver_home: String,
    jobserver_output_dir: String,
}

#[derive(Debug)]
struct TestJob {
    repo: String,
    branch: String,
    commit: String,
    /// None when the legacy build-from-repo path applies; Some(id) for
    /// a kernel-store entry like "debian/forky".
    kernel: Option<String>,
    test_path: String,
    subtests: Vec<String>,
}

const SSH_OPTS: &[&str] = &[
    "-o", "ControlMaster=auto",
    "-o", "ControlPath=~/.ssh/master-%r@%h:%p",
    "-o", "ControlPersist=10m",
    "-o", "ServerAliveInterval=10",
];

fn log_line(msg: &str) {
    eprintln!("[{}] {}", Local::now().format("%H:%M:%S"), msg);
}

macro_rules! log {
    ($($arg:tt)*) => { $crate::log_line(&format!($($arg)*)) };
}

/// Run `cmd` with stdio inherited. Returns the exit status — caller
/// decides whether non-zero is fatal.
fn run_inherit(cmd: &mut Command) -> Result<ExitStatus> {
    let pretty = format!("{:?}", cmd);
    cmd.stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to spawn: {}", pretty))
}

/// Run `cmd`, treating a non-zero exit as an error. The cmd's debug
/// repr (which includes the program and all args) is the context for
/// both spawn failures and non-zero exits — usually more useful than a
/// hand-crafted string at the call site.
fn run_check(cmd: &mut Command) -> Result<()> {
    let pretty = format!("{:?}", cmd);
    let st = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to spawn: {}", pretty))?;
    if !st.success() {
        bail!("{} exited {:?}", pretty, st.code());
    }
    Ok(())
}

/// Run `cmd` capturing stdout, returning an error on non-zero exit.
fn run_check_capture(cmd: &mut Command) -> Result<Output> {
    let pretty = format!("{:?}", cmd);
    let out = cmd
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to spawn: {}", pretty))?;
    if !out.status.success() {
        bail!("{} exited {:?}", pretty, out.status.code());
    }
    Ok(out)
}

/// Retry a fallible operation forever, logging each failure with the
/// supplied label. The bash ssh_retry analog.
fn retry_forever<T>(label: &str, mut op: impl FnMut() -> Result<T>) -> T {
    loop {
        match op() {
            Ok(t) => return t,
            Err(e) => {
                log!("{}: {:#}, retrying", label, e);
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn ssh_cmd(host: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(SSH_OPTS).arg(host).args(args);
    cmd
}

fn ssh_retry_capture(host: &str, args: &[&str]) -> Output {
    let label = format!("ssh {} {}", host, args.join(" "));
    retry_forever(&label, || run_check_capture(&mut ssh_cmd(host, args)))
}

fn ssh_retry_run(host: &str, args: &[&str]) {
    let label = format!("ssh {} {}", host, args.join(" "));
    retry_forever(&label, || run_check(&mut ssh_cmd(host, args)))
}

/// Fetch the jobserver's .ktestrc and extract the keys the worker cares
/// about. The remote file is a bash-sourceable config; we only need a
/// couple of values, so we grep for them rather than evaluating bash.
fn fetch_jobserver_rc(jobserver: &str) -> Result<JobserverRc> {
    let out = ssh_retry_capture(jobserver, &["cat", ".ktestrc"]);
    let text = String::from_utf8(out.stdout)
        .with_context(|| format!("{}:.ktestrc not utf-8", jobserver))?;

    let mut rc = JobserverRc::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("JOBSERVER_HOME=") {
            rc.jobserver_home = unquote(rest);
        } else if let Some(rest) = line.strip_prefix("JOBSERVER_OUTPUT_DIR=") {
            rc.jobserver_output_dir = unquote(rest);
        }
    }
    if rc.jobserver_home.is_empty() {
        bail!("JOBSERVER_HOME not set in {}:.ktestrc", jobserver);
    }
    if rc.jobserver_output_dir.is_empty() {
        bail!("JOBSERVER_OUTPUT_DIR not set in {}:.ktestrc", jobserver);
    }
    Ok(rc)
}

/// Strip a single layer of bash-style quoting (single or double).
fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
        .to_string()
}

/// Wait until the jobserver has enough free memory to take on more
/// work. Matches the old wait-for-mem.sh logic: poll /proc/meminfo,
/// proceed once MemAvailable >= MemTotal/2.
fn wait_for_server_mem(jobserver: &str) {
    loop {
        let out = ssh_retry_capture(jobserver, &["cat", "/proc/meminfo"]);
        let text = String::from_utf8_lossy(&out.stdout);
        if jobserver_has_mem(&text) {
            return;
        }
        log!("{}: memory pressure, waiting", jobserver);
        let n: u64 = rand::thread_rng().gen_range(0..20);
        thread::sleep(Duration::from_secs(n));
    }
}

fn jobserver_has_mem(meminfo: &str) -> bool {
    let field = |name: &str| -> Option<u64> {
        meminfo
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|s| s.parse().ok())
    };
    match (field("MemTotal:"), field("MemAvailable:")) {
        (Some(total), Some(available)) => available >= total / 2,
        // Can't parse → don't block forever on a parse error.
        _ => true,
    }
}

/// Ask the jobserver for the next job for us. Returns Ok(None) when no
/// jobs are available (jobserver replied but no TEST_JOB line).
fn poll_for_job(
    jobserver: &str,
    hostname: &str,
    workdir: &str,
    verbose: bool,
) -> Result<Option<TestJob>> {
    let mut args: Vec<&str> = vec!["get-test-job"];
    if verbose {
        args.push("-v");
    }
    args.push(hostname);
    args.push(workdir);
    let out = ssh_retry_capture(jobserver, &args);
    let text = String::from_utf8(out.stdout)
        .with_context(|| format!("get-test-job output from {} not utf-8", jobserver))?;

    text.lines()
        .find(|l| l.starts_with("TEST_JOB "))
        .map(parse_test_job)
        .transpose()
}

fn parse_test_job(line: &str) -> Result<TestJob> {
    // Format: TEST_JOB <repo> <branch> <commit> <kernel-or-`-`> <test> <subtests...>
    let missing = |field: &'static str| anyhow!("missing {} in job line: {:?}", field, line);
    let mut fields = line.split_whitespace();
    let tag = fields.next().ok_or_else(|| missing("TEST_JOB tag"))?;
    if tag != "TEST_JOB" {
        bail!("expected TEST_JOB tag, got {:?} in job line: {:?}", tag, line);
    }
    let repo = fields.next().ok_or_else(|| missing("repo"))?.to_string();
    let branch = fields.next().ok_or_else(|| missing("branch"))?.to_string();
    let commit = fields.next().ok_or_else(|| missing("commit"))?.to_string();
    let kernel_field = fields.next().ok_or_else(|| missing("kernel"))?;
    let test_path = fields.next().ok_or_else(|| missing("test"))?.to_string();
    let subtests: Vec<String> = fields.map(String::from).collect();
    if subtests.is_empty() {
        bail!("no subtests in job line: {:?}", line);
    }
    Ok(TestJob {
        repo,
        branch,
        commit,
        kernel: (kernel_field != "-").then(|| kernel_field.to_string()),
        test_path,
        subtests,
    })
}

fn jobserver_repo_url(jobserver: &str, jobserver_home: &str, repo: &str) -> String {
    // scp-like syntax: git uses ssh transport, and the absolute path
    // is unambiguous. The ssh:// URL form would produce a double
    // slash for absolute JOBSERVER_HOME paths (ssh://host//abs/path),
    // which git passes through verbatim and the remote rejects.
    format!("{}:{}/{}", jobserver, jobserver_home, repo)
}

/// Recursively remove a single filesystem entry — directories via
/// remove_dir_all, anything else (file or symlink) via remove_file. We
/// never want to follow a symlink-to-dir into something we don't own.
fn rm(path: &Path) -> Result<()> {
    let result = if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.with_context(|| format!("rm {}", path.display()))
}

/// Wipe everything in cwd. Used on cross-repo switch (the previous repo's
/// checkout, the ktest-out kernel cache keyed to its source tree, and
/// any leftover require-git deps all become invalid at once).
fn wipe_state_dir() -> Result<()> {
    for entry in fs::read_dir(".")? {
        rm(&entry?.path())?;
    }
    Ok(())
}

/// Place a checkout of <repo> at <commit> in ./<repo>/. If the
/// directory already exists (same-repo consecutive jobs), just fetch
/// and check out.
fn ensure_state_dir_checkout(
    jobserver: &str,
    jobserver_home: &str,
    repo: &str,
    commit: &str,
) -> Result<()> {
    let url = jobserver_repo_url(jobserver, jobserver_home, repo);
    let checkout = PathBuf::from(repo);

    if !checkout.exists() {
        log!("Cloning {} into {}", repo, checkout.display());
        run_check(Command::new("git").arg("clone").arg(&url).arg(&checkout))?;
    }

    // Fetch the specific commit (it may not have been on a ref the
    // initial clone pulled).
    log!("Fetching {} {}", repo, commit);
    git_fetch_retry(&checkout, &url, commit)?;

    run_check(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["checkout", "-f", "FETCH_HEAD"]),
    )?;

    // Defensive: make sure HEAD actually matches the requested commit.
    let out = run_check_capture(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["rev-parse", "HEAD"]),
    )?;
    let head = String::from_utf8(out.stdout)
        .with_context(|| format!("git rev-parse HEAD output in {} not utf-8", checkout.display()))?
        .trim()
        .to_string();
    if head != commit {
        bail!(
            "{}: HEAD is {} but the job asked for {}",
            checkout.display(),
            head,
            commit
        );
    }
    Ok(())
}

fn git_fetch_retry(checkout: &Path, url: &str, commit: &str) -> Result<()> {
    loop {
        let st = run_inherit(
            Command::new("git")
                .arg("-C")
                .arg(checkout)
                .arg("fetch")
                .arg(url)
                .arg(commit),
        )?;
        let label = || {
            format!(
                "git fetch {} {} (into {})",
                url,
                commit,
                checkout.display()
            )
        };
        match st.code() {
            Some(0) => return Ok(()),
            Some(1) => bail!("{} exited 1 (commit not found?)", label()),
            Some(128) => bail!("{} exited 128 (fatal repo error)", label()),
            other => {
                log!("{} exited {:?}, retrying", label(), other);
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Clean ktest-out for a fresh test run, preserving the kernel build
/// cache (ktest-out/kernel*) for incremental kbuild on same-repo jobs.
fn clean_ktest_out() -> Result<()> {
    let kt = Path::new("ktest-out");
    if !kt.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(kt)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("kernel") {
            continue;
        }
        rm(&entry.path())?;
    }
    Ok(())
}

/// Compress every plain *log file under ktest-out/out via brotli --rm.
fn compress_logs() -> Result<()> {
    let out = Path::new("ktest-out/out");
    if !out.exists() {
        return Ok(());
    }
    let paths: Vec<PathBuf> = WalkDir::new(out)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.file_name().to_string_lossy().ends_with("log"))
        .map(|e| e.into_path())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new("brotli");
    cmd.args(["--rm", "-9"]).args(&paths);
    run_check(&mut cmd)
}

/// Tar up ktest-out/out and pipe it over ssh to the jobserver, which
/// extracts under JOBSERVER_OUTPUT_DIR. Retries on failure.
fn upload_results(jobserver: &str, jobserver_output_dir: &str, commit: &str) -> Result<()> {
    let out = Path::new("ktest-out/out");
    let staged = PathBuf::from(format!("ktest-out/{}", commit));
    // Renaming to the commit-scoped dir lets the receiving tar extract
    // straight into JOBSERVER_OUTPUT_DIR/<commit>/.
    fs::rename(out, &staged).with_context(|| format!("rename {:?} -> {:?}", out, staged))?;

    let label = format!("upload to {}", jobserver);
    retry_forever(&label, || {
        let mut tar = Command::new("tar")
            .args(["--create", "--file", "-"])
            .arg(commit)
            .current_dir("ktest-out")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("spawn tar")?;
        let tar_stdout = tar.stdout.take().context("tar has no stdout pipe")?;

        let ssh_st = Command::new("ssh")
            .args(SSH_OPTS)
            .arg(jobserver)
            .arg(format!("(cd {}; tar --extract --file -)", jobserver_output_dir))
            .stdin(Stdio::from(tar_stdout))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("spawn ssh for tar pipe")?;
        let tar_st = tar.wait().context("wait on tar")?;

        if tar_st.success() && ssh_st.success() {
            Ok(())
        } else {
            bail!("tar {:?}, ssh {:?}", tar_st.code(), ssh_st.code())
        }
    });

    // Always rename back so the next iteration finds ktest-out/out in
    // the expected place.
    fs::rename(&staged, out)
        .with_context(|| format!("rename {:?} -> {:?}", staged, out))?;
    Ok(())
}

fn upload_lcov(
    jobserver: &str,
    jobserver_output_dir: &str,
    commit: &str,
    test_name: &str,
    hostname: &str,
) -> Result<()> {
    if !Path::new("ktest-out/gcov.0").exists() {
        return Ok(());
    }
    log!("Sending gcov results to jobserver");

    let stamp = Local::now().format("%+").to_string();
    let lcov_path = format!("ktest-out/out/lcov.partial.{}.{}.{}", test_name, hostname, stamp);

    run_check(
        Command::new("lcov")
            .args(["--capture", "--quiet", "--directory", "ktest-out/gcov.0", "--output-file"])
            .arg(&lcov_path),
    )?;

    // Strip the cwd prefix so paths in the lcov file are repo-relative.
    let cwd = env::current_dir()?;
    run_check(
        Command::new("sed")
            .arg("-i")
            .arg("-e")
            .arg(format!("s_{}/__", cwd.display()))
            .arg(&lcov_path),
    )?;

    let output_path = format!("{}/{}", jobserver_output_dir, commit);
    let dest = format!("{}:{}", jobserver, output_path);
    run_check(Command::new("scp").args(SSH_OPTS).arg(&lcov_path).arg(&dest))?;

    ssh_retry_run(
        jobserver,
        &[&format!("(cd {}; touch lcov-stale)", output_path)],
    );
    ssh_retry_run(jobserver, &["update-lcov", commit]);
    Ok(())
}

/// Path of a subtest's status file, given the test_name and subtest.
fn status_path(test_name: &str, subtest: &str) -> PathBuf {
    let fname = subtest.replace('/', ".");
    PathBuf::from(format!("ktest-out/out/{}.{}/status", test_name, fname))
}

fn is_in_progress(test_name: &str, subtest: &str) -> bool {
    fs::read_to_string(status_path(test_name, subtest))
        .map(|s| s.contains("IN PROGRESS"))
        .unwrap_or(false)
}

fn run_test_job(
    jobserver: &str,
    rc: &JobserverRc,
    ktest_dir: &Path,
    job: &TestJob,
    hostname: &str,
) -> Result<()> {
    let test_path = ktest_dir.join("tests").join(&job.test_path);
    let test_path = test_path
        .canonicalize()
        .with_context(|| format!("test path {:?} doesn't exist", test_path))?;

    // test_name is the test path with '/' -> '.' and (when running under a
    // specific kernel-store entry) a '@<kernel>' suffix, so two kernels
    // against the same commit don't clobber each other's result dirs.
    // No kernel suffix preserves the legacy on-disk layout for the
    // build-from-repo path.
    let test_name = {
        let base = job
            .test_path
            .strip_suffix(".ktest")
            .unwrap_or(&job.test_path)
            .replace('/', ".");
        match &job.kernel {
            Some(k) => format!("{}@{}", base, k.replace('/', "_")),
            None => base,
        }
    };

    let kernel_clause = job
        .kernel
        .as_ref()
        .map(|k| format!(" under kernel {}", k))
        .unwrap_or_default();
    log!(
        "Running test {} for {} branch {} commit {}{}",
        test_name, job.repo, job.branch, job.commit, kernel_clause
    );

    // State-dir lifecycle: if cwd doesn't already have a checkout for
    // this job's repo, wipe everything (previous repo's checkout +
    // its kernel build cache + any leftover deps).
    if !Path::new(&job.repo).exists() {
        log!("Repo switch (no ./{} in state dir) — wiping state dir", job.repo);
        wipe_state_dir()?;
    }

    ensure_state_dir_checkout(jobserver, &rc.jobserver_home, &job.repo, &job.commit)?;

    clean_ktest_out()?;
    fs::create_dir_all("ktest-out/out")?;

    // Mark all subtests IN_PROGRESS so the result upload always reports
    // a status — even if the VM dies before producing one.
    for t in &job.subtests {
        let path = status_path(&test_name, t);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, "IN PROGRESS\n")?;
    }

    // The supervisor is a small C helper built from ktest's lib/.
    // Idempotent; safe to invoke every job.
    run_check(Command::new("make").arg("-C").arg(ktest_dir.join("lib")).arg("supervisor"))?;

    let state_dir = env::current_dir()?;
    let checkout = state_dir.join(&job.repo);
    let supervisor = ktest_dir.join("lib").join("supervisor");

    let mut remaining: Vec<String> = job.subtests.clone();
    while !remaining.is_empty() {
        // Clean per-iteration gcov dump (lib/libktest sets up gcov.0 etc.).
        for entry in fs::read_dir("ktest-out")? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with("gcov.") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }

        let stamp = Local::now().format("%+").to_string();
        let full_log = format!("{}.{}.{}.log", test_name, hostname, stamp);

        for t in &remaining {
            let fname = t.replace('/', ".");
            let link = format!("ktest-out/out/{}.{}/full_log.br", test_name, fname);
            let target = format!("../{}.br", full_log);
            let _ = fs::remove_file(&link);
            symlink(&target, &link)
                .with_context(|| format!("symlink {} -> {}", link, target))?;
        }

        log!("Running test {} {}", job.test_path, remaining.join(" "));

        let mut cmd = Command::new(&supervisor);
        cmd.args([
            "-T", "1200", "-f", &full_log, "-S", "-F",
            "-b", &test_name, "-o", "ktest-out/out", "--",
        ]);
        match &job.kernel {
            Some(k) => {
                cmd.arg(ktest_dir.join("ktest"))
                    .arg("-k")
                    .arg(k)
                    .arg("run")
                    .arg(&test_path);
            }
            None => {
                cmd.arg(ktest_dir.join("build-test-kernel"))
                    .arg("run")
                    .arg("-k")
                    .arg(&checkout)
                    .arg("-P")
                    .arg(&test_path);
            }
        }
        cmd.args(&remaining);

        // Wire require-git in the tests to the state dir, so deps land
        // here instead of polluting the ktest source tree.
        cmd.env("ktest_deps_dir", &state_dir);

        log!("supervisor cmd: {:?}", cmd);
        // The supervisor's exit code reflects test failures, which already
        // show up in the status files. Only the spawn failure is fatal.
        run_inherit(&mut cmd)?;

        // First subtest didn't finish → mark it NOT STARTED and drop it
        // from the retry set (next iteration would just hit it again).
        // Remaining subtests that are still IN PROGRESS get retried.
        let (first, rest) = remaining.split_first().unwrap();
        if is_in_progress(&test_name, first) {
            fs::write(status_path(&test_name, first), "NOT STARTED\n")?;
        }
        let next: Vec<String> = rest
            .iter()
            .filter(|t| is_in_progress(&test_name, t))
            .cloned()
            .collect();

        log!("Compressing output");
        compress_logs()?;

        log!("Sending results to jobserver");
        upload_results(jobserver, &rc.jobserver_output_dir, &job.commit)?;
        upload_lcov(jobserver, &rc.jobserver_output_dir, &job.commit, &test_name, hostname)?;
        ssh_retry_run(jobserver, &["gen-commit-summary", &job.commit]);

        remaining = next;
    }
    Ok(())
}

/// Path to the ktest source tree, baked in at build time. Lets the
/// binary find tests/, lib/supervisor, etc. without scanning relative
/// to its install location. Fine for now since every worker host
/// builds locally via ci/ci-worker's nix-shell.
const KTEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn main() -> Result<()> {
    let args = Args::parse();

    let ktest_dir = Path::new(KTEST_DIR);
    let rc = fetch_jobserver_rc(&args.jobserver)?;
    let hostname = hostname::get()
        .context("gethostname")?
        .to_string_lossy()
        .into_owned();
    let cwd = env::current_dir().context("getting cwd for workdir basename")?;
    let workdir = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("cwd {} has no usable basename", cwd.display()))?
        .to_string();

    loop {
        log!("Getting test job");
        let job = loop {
            if let Some(j) = poll_for_job(&args.jobserver, &hostname, &workdir, args.verbose)? {
                break j;
            }
            log!("test-git-branch: No test job available");
            if args.once {
                std::process::exit(1);
            }
            // Soft-start jitter (matches the bash $RANDOM % 100).
            let n: u64 = rand::thread_rng().gen_range(0..100);
            thread::sleep(Duration::from_secs(n));
        };

        log!(
            "Got job {} {} {} {:?} {} {:?}",
            job.repo, job.branch, job.commit, job.kernel, job.test_path, job.subtests
        );

        wait_for_server_mem(&args.jobserver);

        if let Err(e) = run_test_job(&args.jobserver, &rc, ktest_dir, &job, &hostname) {
            log!(
                "run_test_job failed for {} {} {} test {}: {:#}",
                job.repo, job.branch, job.commit, job.test_path, e
            );
            thread::sleep(Duration::from_secs(10));
        }

        if args.once {
            break;
        }
    }

    Ok(())
}
