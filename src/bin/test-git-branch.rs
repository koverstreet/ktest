// CI worker: poll a jobserver for test jobs, run them, ship results back.
//
// Replaces ci/test-git-branch.sh. Same CLI surface:
//
//     test-git-branch [-o] [-v] JOBSERVER
//
// Differences from the old script:
//
//  * State-dir model. Cwd is the worker's state dir, not a kernel clone.
//    The repo named in the job is cloned into ./<repo>/ on demand, using
//    a host-shared bare cache at $HOME/git/<repo>.git to keep
//    re-clones cheap. Same-repo consecutive jobs keep the checkout and
//    the kernel build cache; switching repos wipes the state dir
//    (kernel build artifacts are keyed to a specific source tree).
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
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPath=~/.ssh/master-%r@%h:%p",
    "-o",
    "ControlPersist=10m",
    "-o",
    "ServerAliveInterval=10",
];

fn log(msg: &str) {
    eprintln!("[{}] {}", Local::now().format("%H:%M:%S"), msg);
}

/// Run a Command with stdout/stderr inherited (streamed to our caller).
/// The `OK?` of the wrapped Command::status() error gets passed through;
/// callers decide whether a non-zero exit is fatal.
fn run_inherit(cmd: &mut Command) -> Result<ExitStatus> {
    let pretty = format!("{:?}", cmd);
    cmd.stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to spawn: {}", pretty))
}

/// Run a Command capturing stdout (stderr inherited).
fn run_capture(cmd: &mut Command) -> Result<Output> {
    let pretty = format!("{:?}", cmd);
    cmd.stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to spawn: {}", pretty))
}

/// `ssh <opts> HOST <args...>` capturing stdout. Retries forever on failure.
fn ssh_retry_capture(host: &str, args: &[&str]) -> Output {
    loop {
        let mut cmd = Command::new("ssh");
        cmd.args(SSH_OPTS).arg(host).args(args);
        match run_capture(&mut cmd) {
            Ok(o) if o.status.success() => return o,
            Ok(o) => log(&format!(
                "ssh {} failed (exit {:?}), retrying",
                args.join(" "),
                o.status.code()
            )),
            Err(e) => log(&format!("ssh {} failed ({}), retrying", args.join(" "), e)),
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// `ssh <opts> HOST <args...>` with stdio inherited. Retries forever
/// on failure (matching the bash script's ssh_retry).
fn ssh_retry_run(host: &str, args: &[&str]) -> ExitStatus {
    loop {
        let mut cmd = Command::new("ssh");
        cmd.args(SSH_OPTS).arg(host).args(args);
        match run_inherit(&mut cmd) {
            Ok(s) if s.success() => return s,
            Ok(s) => log(&format!(
                "ssh {} failed (exit {:?}), retrying",
                args.join(" "),
                s.code()
            )),
            Err(e) => log(&format!("ssh {} failed ({}), retrying", args.join(" "), e)),
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// Fetch the jobserver's .ktestrc and extract the keys the worker cares
/// about. The remote file is a bash-sourceable config; we only need a
/// couple of values, so we grep for them rather than evaluating bash.
fn fetch_jobserver_rc(jobserver: &str) -> Result<JobserverRc> {
    let out = ssh_retry_capture(jobserver, &["cat", ".ktestrc"]);
    let text = String::from_utf8(out.stdout).context("jobserver .ktestrc not utf-8")?;

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
        bail!("JOBSERVER_HOME not set in jobserver's .ktestrc");
    }
    if rc.jobserver_output_dir.is_empty() {
        bail!("JOBSERVER_OUTPUT_DIR not set in jobserver's .ktestrc");
    }
    Ok(rc)
}

/// Strip a single layer of bash-style quoting (single or double).
fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn wait_for_server_mem(jobserver: &str, ktest_dir: &Path) {
    let script = format!("{}/ci/wait-for-mem.sh", ktest_dir.display());
    ssh_retry_run(jobserver, &[&script]);
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
    let text = String::from_utf8(out.stdout).context("get-test-job output not utf-8")?;

    // get-test-job prints diagnostic lines on stderr (via -v) and a
    // single TEST_JOB line on stdout when it has work. Anything else
    // means "no job".
    for line in text.lines() {
        if line.starts_with("TEST_JOB ") {
            return Ok(Some(parse_test_job(line)?));
        }
    }
    Ok(None)
}

fn parse_test_job(line: &str) -> Result<TestJob> {
    // Format: TEST_JOB <repo> <branch> <commit> <kernel-or-`-`> <test> <subtests...>
    let mut fields = line.split_whitespace();
    let tag = fields.next().ok_or_else(|| anyhow!("empty job line"))?;
    if tag != "TEST_JOB" {
        bail!("expected TEST_JOB tag, got {:?}", tag);
    }
    let repo = fields.next().ok_or_else(|| anyhow!("missing repo"))?;
    let branch = fields.next().ok_or_else(|| anyhow!("missing branch"))?;
    let commit = fields.next().ok_or_else(|| anyhow!("missing commit"))?;
    let kernel_field = fields.next().ok_or_else(|| anyhow!("missing kernel"))?;
    let test = fields.next().ok_or_else(|| anyhow!("missing test"))?;
    let subtests: Vec<String> = fields.map(String::from).collect();
    if subtests.is_empty() {
        bail!("no subtests in job line");
    }
    Ok(TestJob {
        repo: repo.to_string(),
        branch: branch.to_string(),
        commit: commit.to_string(),
        kernel: if kernel_field == "-" {
            None
        } else {
            Some(kernel_field.to_string())
        },
        test_path: test.to_string(),
        subtests,
    })
}

/// Ensure $HOME/git/<repo>.git exists and is up to date. The bare cache
/// is host-shared across all worker state dirs, so per-state-dir clones
/// reference it instead of re-downloading the whole history.
fn ensure_bare_cache(jobserver: &str, jobserver_home: &str, repo: &str) -> Result<PathBuf> {
    let home = env::var("HOME").context("HOME not set")?;
    let bare = PathBuf::from(home).join("git").join(format!("{}.git", repo));

    if !bare.exists() {
        fs::create_dir_all(bare.parent().unwrap())?;
        let url = format!("ssh://{}/{}/{}", jobserver, jobserver_home, repo);
        log(&format!("Bare-cloning {} -> {}", url, bare.display()));
        let st = run_inherit(
            Command::new("git")
                .arg("clone")
                .arg("--bare")
                .arg(&url)
                .arg(&bare),
        )?;
        if !st.success() {
            bail!("git clone --bare failed");
        }
    } else {
        log(&format!("Updating bare cache {}", bare.display()));
        let st = run_inherit(Command::new("git").arg("-C").arg(&bare).arg("fetch"))?;
        if !st.success() {
            // Fetch failure is not fatal — the cache may still satisfy
            // the requested commit. Log and proceed.
            log("bare-cache fetch failed; proceeding with existing cache");
        }
    }
    Ok(bare)
}

/// Wipe everything in cwd. Used on cross-repo switch (the previous repo's
/// checkout, the ktest-out kernel cache keyed to its source tree, and
/// any leftover require-git deps all become invalid at once).
fn wipe_state_dir() -> Result<()> {
    for entry in fs::read_dir(".")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("rm -rf {}", path.display()))?;
        } else {
            fs::remove_file(&path)
                .with_context(|| format!("rm {}", path.display()))?;
        }
    }
    Ok(())
}

/// Place a checkout of <repo> at <commit> in ./<repo>/, sharing objects
/// with the bare cache. If the directory already exists (same-repo
/// consecutive jobs), just fetch and check out.
fn ensure_state_dir_checkout(
    bare: &Path,
    jobserver: &str,
    jobserver_home: &str,
    repo: &str,
    commit: &str,
) -> Result<()> {
    let url = format!("ssh://{}/{}/{}", jobserver, jobserver_home, repo);
    let checkout = PathBuf::from(repo);

    if !checkout.exists() {
        log(&format!("Cloning {} into {}", repo, checkout.display()));
        let st = run_inherit(
            Command::new("git")
                .arg("clone")
                .arg("--reference")
                .arg(bare)
                .arg(&url)
                .arg(&checkout),
        )?;
        if !st.success() {
            bail!("git clone --reference failed");
        }
    }

    // Fetch the specific commit (it may not have been on a ref the
    // initial clone pulled, or the cache may be stale).
    log(&format!("Fetching {} {}", repo, commit));
    git_fetch_retry(&checkout, &url, commit)?;

    let st = run_inherit(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .arg("checkout")
            .arg("-f")
            .arg("FETCH_HEAD"),
    )?;
    if !st.success() {
        bail!("git checkout FETCH_HEAD failed");
    }

    // Defensive: make sure HEAD actually matches the requested commit.
    let out = run_capture(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .arg("rev-parse")
            .arg("HEAD"),
    )?;
    let head = String::from_utf8(out.stdout)?.trim().to_string();
    if head != commit {
        bail!("HEAD {} != requested commit {}", head, commit);
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
        match st.code() {
            Some(0) => return Ok(()),
            Some(1) => bail!("git fetch returned 1 (commit not found?)"),
            Some(128) => bail!("git fetch returned 128 (fatal repo error)"),
            other => {
                log(&format!("git fetch returned {:?}, retrying", other));
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Clean ktest-out for a fresh test run, preserving the kernel build
/// cache (ktest-out/kernel*) for incremental kbuild on same-repo jobs.
fn clean_ktest_out() -> Result<()> {
    let kt = PathBuf::from("ktest-out");
    if !kt.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&kt)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("kernel") {
            continue;
        }
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Compress every plain *log file under ktest-out/out via brotli --rm.
fn compress_logs() -> Result<()> {
    let out = PathBuf::from("ktest-out/out");
    if !out.exists() {
        return Ok(());
    }
    let mut paths = Vec::new();
    for entry in WalkDir::new(&out) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with("log") {
                paths.push(entry.into_path());
            }
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new("brotli");
    cmd.arg("--rm").arg("-9");
    for p in &paths {
        cmd.arg(p);
    }
    let st = run_inherit(&mut cmd)?;
    if !st.success() {
        bail!("brotli returned {:?}", st.code());
    }
    Ok(())
}

/// Tar up ktest-out/out and pipe it over ssh to the jobserver, which
/// extracts under JOBSERVER_OUTPUT_DIR. Retries on failure.
fn upload_results(jobserver: &str, jobserver_output_dir: &str, commit: &str) -> Result<()> {
    let out = PathBuf::from("ktest-out/out");
    let staged = PathBuf::from(format!("ktest-out/{}", commit));
    // Renaming to the commit-scoped dir lets the receiving tar extract
    // straight into JOBSERVER_OUTPUT_DIR/<commit>/.
    fs::rename(&out, &staged).with_context(|| format!("rename {:?} -> {:?}", out, staged))?;

    let result = (|| -> Result<()> {
        loop {
            let mut tar = Command::new("tar")
                .arg("--create")
                .arg("--file")
                .arg("-")
                .arg(commit)
                .current_dir("ktest-out")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .context("spawn tar")?;
            let tar_stdout = tar.stdout.take().unwrap();

            let mut ssh = Command::new("ssh");
            ssh.args(SSH_OPTS).arg(jobserver).arg(format!(
                "(cd {}; tar --extract --file -)",
                jobserver_output_dir
            ));
            let ssh_st = ssh
                .stdin(Stdio::from(tar_stdout))
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .context("spawn ssh for tar pipe")?;

            let tar_st = tar.wait().context("wait on tar")?;

            if tar_st.success() && ssh_st.success() {
                return Ok(());
            }
            log(&format!(
                "result upload failed (tar {:?}, ssh {:?}), retrying",
                tar_st.code(),
                ssh_st.code()
            ));
            thread::sleep(Duration::from_secs(1));
        }
    })();

    // Always rename back, even on error, so the next iteration finds
    // ktest-out/out in the expected place.
    let _ = fs::rename(&staged, &out);
    result
}

fn upload_lcov(
    jobserver: &str,
    jobserver_output_dir: &str,
    commit: &str,
    test_name: &str,
    hostname: &str,
) -> Result<()> {
    if !PathBuf::from("ktest-out/gcov.0").exists() {
        return Ok(());
    }
    log("Sending gcov results to jobserver");

    let stamp = Local::now().format("%+").to_string();
    let lcov_path = format!("ktest-out/out/lcov.partial.{}.{}.{}", test_name, hostname, stamp);

    let st = run_inherit(
        Command::new("lcov")
            .arg("--capture")
            .arg("--quiet")
            .arg("--directory")
            .arg("ktest-out/gcov.0")
            .arg("--output-file")
            .arg(&lcov_path),
    )?;
    if !st.success() {
        bail!("lcov returned {:?}", st.code());
    }

    // Strip the cwd prefix so paths in the lcov file are repo-relative.
    let cwd = env::current_dir()?;
    let cwd_str = cwd.to_string_lossy();
    let st = run_inherit(
        Command::new("sed")
            .arg("-i")
            .arg("-e")
            .arg(format!("s_{}/__", cwd_str))
            .arg(&lcov_path),
    )?;
    if !st.success() {
        bail!("sed on lcov file returned {:?}", st.code());
    }

    let output_path = format!("{}/{}", jobserver_output_dir, commit);
    let dest = format!("{}:{}", jobserver, output_path);
    let st = run_inherit(Command::new("scp").args(SSH_OPTS).arg(&lcov_path).arg(&dest))?;
    if !st.success() {
        bail!("scp lcov file returned {:?}", st.code());
    }

    ssh_retry_run(
        jobserver,
        &[&format!("(cd {}; touch lcov-stale)", output_path)],
    );
    ssh_retry_run(jobserver, &["update-lcov", commit]);
    Ok(())
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

    let mut test_name = job
        .test_path
        .strip_suffix(".ktest")
        .unwrap_or(&job.test_path)
        .replace('/', ".");
    // Per-subtest result dirs key off (test, kernel) so two kernels
    // against the same commit don't clobber. The bare TEST_NAME with
    // no kernel suffix preserves the legacy on-disk layout for the
    // build-from-repo path.
    if let Some(k) = &job.kernel {
        test_name = format!("{}@{}", test_name, k.replace('/', "_"));
    }

    if let Some(k) = &job.kernel {
        log(&format!(
            "Running test {} for {} branch {} commit {} under kernel {}",
            test_name, job.repo, job.branch, job.commit, k
        ));
    } else {
        log(&format!(
            "Running test {} for {} branch {} commit {}",
            test_name, job.repo, job.branch, job.commit
        ));
    }

    // State-dir lifecycle: if cwd doesn't already have a checkout for
    // this job's repo, wipe everything (previous repo's checkout +
    // its kernel build cache + any leftover deps).
    if !PathBuf::from(&job.repo).exists() {
        log(&format!(
            "Repo switch (no ./{} in state dir) — wiping state dir",
            job.repo
        ));
        wipe_state_dir()?;
    }

    let bare = ensure_bare_cache(jobserver, &rc.jobserver_home, &job.repo)?;
    ensure_state_dir_checkout(&bare, jobserver, &rc.jobserver_home, &job.repo, &job.commit)?;

    clean_ktest_out()?;
    fs::create_dir_all("ktest-out/out")?;

    // Mark all subtests IN_PROGRESS so the result upload always reports
    // a status — even if the VM dies before producing one.
    for t in &job.subtests {
        let fname = t.replace('/', ".");
        let dir = format!("ktest-out/out/{}.{}", test_name, fname);
        fs::create_dir_all(&dir)?;
        fs::write(format!("{}/status", dir), "IN PROGRESS\n")?;
    }

    // The supervisor is a small C helper built from ktest's lib/.
    // Idempotent; safe to invoke every job.
    let st = run_inherit(
        Command::new("make")
            .arg("-C")
            .arg(ktest_dir.join("lib"))
            .arg("supervisor"),
    )?;
    if !st.success() {
        bail!("supervisor build failed");
    }

    let state_dir = env::current_dir()?;
    let checkout = state_dir.join(&job.repo);

    let mut remaining: Vec<String> = job.subtests.clone();
    while !remaining.is_empty() {
        // Clean per-iteration gcov dump (lib/libktest sets up gcov.0 etc.).
        for entry in glob_dirs("ktest-out", "gcov.")? {
            let _ = fs::remove_dir_all(entry);
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

        log(&format!("Running test {} {}", job.test_path, remaining.join(" ")));

        let supervisor = ktest_dir.join("lib").join("supervisor");
        let mut cmd = Command::new(&supervisor);
        cmd.args(["-T", "1200", "-f", &full_log, "-S", "-F", "-b", &test_name, "-o", "ktest-out/out", "--"]);

        // Dispatch: prebuilt kernel store entry, or build-from-repo.
        match &job.kernel {
            Some(k) => {
                cmd.arg("ktest").arg("-k").arg(k).arg("run").arg(&test_path);
            }
            None => {
                cmd.arg("build-test-kernel")
                    .arg("run")
                    .arg("-k")
                    .arg(&checkout)
                    .arg("-P")
                    .arg(&test_path);
            }
        }
        for t in &remaining {
            cmd.arg(t);
        }

        // Wire require-git in the tests to the state dir, so deps land
        // here instead of polluting the ktest source tree.
        cmd.env("ktest_deps_dir", &state_dir);

        let _ = run_inherit(&mut cmd)?;

        // Determine which subtests still need to run.
        // If the first subtest is still IN PROGRESS, it didn't complete —
        // mark it NOT STARTED so it shows up as a failure rather than
        // perpetually pending, and drop it from the retry set (the next
        // iteration would just hit it again).
        let first = &remaining[0];
        let first_fname = first.replace('/', ".");
        let first_status = format!("ktest-out/out/{}.{}/status", test_name, first_fname);
        if fs::read_to_string(&first_status)
            .map(|s| s.contains("IN PROGRESS"))
            .unwrap_or(false)
        {
            fs::write(&first_status, "NOT STARTED\n")?;
        }

        let mut next: Vec<String> = Vec::new();
        for t in remaining.iter().skip(1) {
            let fname = t.replace('/', ".");
            let status = format!("ktest-out/out/{}.{}/status", test_name, fname);
            if fs::read_to_string(&status)
                .map(|s| s.contains("IN PROGRESS"))
                .unwrap_or(false)
            {
                next.push(t.clone());
            }
        }

        log("Compressing output");
        compress_logs()?;

        log("Sending results to jobserver");
        upload_results(jobserver, &rc.jobserver_output_dir, &job.commit)?;

        upload_lcov(jobserver, &rc.jobserver_output_dir, &job.commit, &test_name, hostname)?;

        ssh_retry_run(jobserver, &["gen-commit-summary", &job.commit]);

        remaining = next;
    }
    Ok(())
}

/// Enumerate entries under `base` whose filename starts with `prefix.`
/// (matches the bash `gcov.*` glob).
fn glob_dirs(base: &str, prefix: &str) -> Result<Vec<PathBuf>> {
    let mut hits = Vec::new();
    let base = PathBuf::from(base);
    if !base.exists() {
        return Ok(hits);
    }
    for entry in fs::read_dir(&base)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(prefix) {
            hits.push(entry.path());
        }
    }
    Ok(hits)
}

fn main() -> Result<()> {
    let args = Args::parse();

    let ktest_dir = {
        let exe = env::current_exe().context("current_exe")?;
        // Binary lives in <ktest>/target/<profile>/test-git-branch.
        // Walk up to the workspace root.
        exe.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("can't derive ktest dir from {:?}", exe))?
    };

    let rc = fetch_jobserver_rc(&args.jobserver)?;

    let hostname = unsafe {
        let mut buf = [0u8; 256];
        if libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) != 0 {
            bail!("gethostname failed");
        }
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..len]).into_owned()
    };
    let workdir = env::current_dir()?
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("cwd has no basename"))?
        .to_string();

    loop {
        log("Getting test job");
        let job = loop {
            match poll_for_job(&args.jobserver, &hostname, &workdir, args.verbose)? {
                Some(j) => break j,
                None => {
                    log("test-git-branch: No test job available");
                    if args.once {
                        std::process::exit(1);
                    }
                    // Soft-start jitter (matches the bash $RANDOM % 100).
                    let n: u64 = rand::thread_rng().gen_range(0..100);
                    thread::sleep(Duration::from_secs(n));
                }
            }
        };

        log(&format!(
            "Got job {} {} {} {:?} {} {:?}",
            job.repo, job.branch, job.commit, job.kernel, job.test_path, job.subtests
        ));

        wait_for_server_mem(&args.jobserver, &ktest_dir);

        if let Err(e) = run_test_job(&args.jobserver, &rc, &ktest_dir, &job, &hostname) {
            log(&format!("run_test_job error: {:#}", e));
            thread::sleep(Duration::from_secs(10));
        }

        if args.once {
            break;
        }
    }

    Ok(())
}
