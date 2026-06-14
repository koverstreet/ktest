// ci-daemon: push-mode CI job runner.
//
// Runs on the jobserver. Builds a jobkit Choir with one executor per
// worker-host slot, then keeps a bounded *window* of jobs submitted:
//   * desired_jobs() yields the newest commits' jobs first, capped
//   * submit that window; as it drains, regenerate and top it back up
//   * write a status snapshot for the cgi
//
// The matrix can be millions of jobs; jobkit only ever holds the
// window (~2000). Each job is one subtest; an executor claims a
// duration-bounded *batch* of one test file's subtests and runs them in
// a single VM — checkout → build supervisor → run → pull results.
//
// Deferred: gcov/lcov upload; the daemon's own git fetch and reconcile
// on branch change (it currently refills only as the window drains).

use anyhow::Result;
use chrono::Utc;
use ci_cgi::jobs::{desired_jobs, Job, JobKey};
use ci_cgi::{
    ciconfig_read, read_test_result, result_basename, subtest_result_key, CiConfig, TestResult,
    TestResultsMap, TestResultsStore, TestStatus,
};
use clap::Parser;
use jobkit::{
    Choir, ClaimedJob, Command, ExecutorConfig, ExecutorHandle, JobId, JobOutcome, JobSpec,
    TaskError,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Bounded job window: jobkit never holds much more than this — the
/// desired matrix itself can be millions of jobs.
const WINDOW: usize = 2000;

/// How often the status snapshot is rewritten, so the cgi's live page
/// stays fresh between reconciles.
const STATUS_INTERVAL: Duration = Duration::from_secs(2);

/// How often to run periodic upkeep — gc-results and gen-avg-duration,
/// the maintenance the old ci-loop used to drive.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// ssh options: connection multiplexing so the daemon's many per-step
/// ssh calls to one worker share a single connection. BatchMode so a
/// missing key fails fast instead of hanging on a prompt.
const SSH_OPTS: &[&str] = &[
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPath=~/.ssh/ci-master-%r@%h:%p",
    "-o",
    "ControlPersist=10m",
    "-o",
    "ServerAliveInterval=10",
    "-o",
    "ConnectTimeout=20",
    "-o",
    "BatchMode=yes",
    // Trust a worker's host key on first contact (the farm is ours);
    // BatchMode can't prompt, so an unknown host would otherwise fail.
    "-o",
    "StrictHostKeyChecking=accept-new",
];

#[derive(Parser)]
#[command(about = "push-mode CI job runner")]
struct Args {
    /// Reconcile once, wait for the jobs to finish, then exit.
    #[arg(long)]
    once: bool,
    /// Cap the number of jobs tracked at once — for cautious testing
    /// (`--limit 1 --once` runs exactly one job end-to-end).
    #[arg(long)]
    limit: Option<usize>,
    /// Drop FailedToRun verdicts at startup so the subtests get
    /// re-emitted. Used to recover after an infra problem (e.g. a
    /// daemon/worker version mismatch) terminalized them spuriously.
    #[arg(long)]
    clear_failed_to_run: bool,
}

fn short_commit(c: &str) -> &str {
    &c[..c.len().min(12)]
}

// --- the executor closure ---

/// Everything the executor closure needs to run one subtest job.
#[derive(Clone)]
struct JobParams {
    repo: String,
    commit: String,
    /// Kernel-store id, or empty for the build-from-repo path.
    kernel: String,
    /// Encoded env overrides ("K1=V1,K2=V2"), empty = none.
    env: String,
    test: String,
    subtest: String,
    /// Public `git fetch` URL the worker pulls the repo from (git://…);
    /// empty if the repo isn't configured.
    repo_url: String,
    /// Daemon-local results dir; pulled results land in `<it>/<commit>/`.
    output_dir: PathBuf,
}

/// An ssh command to `host` running `remote` (one shell command line).
/// `tty` forces a pseudo-terminal (`-tt`): when the daemon's ssh dies,
/// the pty hangs up and SIGHUP reaps the whole remote process group --
/// without it an interrupted job leaks its VM and kernel build.
fn ssh_cmd(host: &str, remote: &str, tty: bool) -> Command {
    let mut c = Command::new("ssh");
    if tty {
        c.arg("-tt");
    }
    // Detach stdin from the daemon's terminal: with -tt, ssh would put
    // that terminal into raw mode, and the operator's Ctrl-C would
    // become a stray byte instead of a SIGINT.
    c.stdin(std::process::Stdio::null());
    c.args(SSH_OPTS).arg(host).arg(remote);
    c
}

/// Shell env-var prefix for the supervisor command: `ktest_deps_dir`
/// pointing at the workspace, plus the per-job env overrides. Encoded
/// env values can't contain spaces, so no quoting is needed.
fn job_env_prefix(env: &str, ws: &str) -> String {
    let mut prefix = format!("ktest_deps_dir=$HOME/{} ", ws);
    for pair in env.split(',').filter(|s| !s.is_empty()) {
        prefix.push_str(pair);
        prefix.push(' ');
    }
    prefix
}

/// Run one ssh step that must succeed; non-zero exit or a spawn failure
/// is infrastructure failure → retry the job.
async fn run_step(
    handle: &ExecutorHandle<JobParams>,
    host: &str,
    remote: &str,
    desc: &str,
) -> Result<(), TaskError> {
    handle.log_line(format!("=== {} ===", desc));
    let status = handle
        .run_command(ssh_cmd(host, remote, false))
        .await
        .map_err(|e| TaskError::Retry(format!("{desc}: {e}")))?;
    if !status.success() {
        return Err(TaskError::Retry(format!(
            "{desc}: ssh exited {:?}",
            status.code()
        )));
    }
    Ok(())
}

/// Compose a daemon-side full_log: batch metadata header + the
/// executor-log slice from `exec_offset_start` to current + the
/// supervisor-written body if any. The slice captures everything
/// ci-daemon saw — pre-supervisor ssh (checkout, build, prepare),
/// the supervisor invocation itself, the pull, anything else
/// in-window. The supervisor body is the VM output the supervisor
/// captured on the worker, pulled back.
fn format_full_log(
    host: &str,
    slot: usize,
    p: &JobParams,
    subtests: &[&str],
    exec_log_path: &std::path::Path,
    exec_offset_start: u64,
    supervisor_body: &[u8],
) -> Vec<u8> {
    let header = format!(
        "# host={} slot={} commit={} kernel={} test={} env={} subtests={}\n",
        host,
        slot,
        p.commit,
        p.kernel,
        p.test,
        p.env,
        subtests.join(","),
    );
    let exec_slice = std::fs::read(exec_log_path)
        .ok()
        .map(|b| {
            let start = (exec_offset_start as usize).min(b.len());
            b[start..].to_vec()
        })
        .unwrap_or_default();
    [
        header.as_bytes(),
        b"# --- ci-daemon executor log for this batch ---\n",
        &exec_slice,
        b"\n# --- supervisor full_log ---\n",
        supervisor_body,
    ]
    .concat()
}

/// Brotli-compress `path` to `<path>.br` and remove the original — the
/// cgi serves test logs as `.br`.
fn brotli_compress(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    let data = std::fs::read(path)?;
    let mut w = brotli::CompressorWriter::new(
        std::fs::File::create(path.with_extension("br"))?,
        4096,
        9,
        22,
    );
    w.write_all(&data)?;
    w.flush()?;
    drop(w);
    std::fs::remove_file(path)
}

/// Run one claimed batch on `handle`'s executor: check out the repo,
/// build the supervisor, then a resume loop — run the not-yet-completed
/// subtests in one VM, and retry any the VM died before reaching.
/// Ported from test-git-branch.sh's run_test_job.
///
/// The supervisor writes each subtest's status itself — it parses the
/// VM output for test-start: Passed/Failed for a test that ran (a
/// kernel panic mid-test is a Failed), FAILED TO RUN for one it could not
/// launch. A subtest it never reached stays IN PROGRESS — those are
/// retried in a fresh VM. A run that completes none of them is an
/// infrastructure failure.
///
/// `batch` is the subtests an executor claimed together — all of one
/// test file at one (user, repo, branch, commit, kernel, env), bounded
/// by subtest_duration_max. They run in a single VM boot.
///
/// Returns Err only on infrastructure failure (a step couldn't run).
async fn run_ktest_job(
    handle: &ExecutorHandle<JobParams>,
    host: &str,
    slot: usize,
    results: &TestResultsStore,
    batch: &[ClaimedJob<JobParams>],
) -> Result<(), TaskError> {
    // Inner does the full_log write inside its resume loop, where
    // it has the per-iter context (which subtests were primary,
    // what slice of the executor log to splice in). The outer
    // wrapper only steps in if a subtest came out the other side
    // without any full_log.br at all — i.e. the inner errored out
    // before it could write one. Those still need *something* to
    // look at on the dashboard.
    let exec_log_offset_start = handle.log_offset();
    let exec_log_path = handle.log_path().to_path_buf();

    let result = run_ktest_job_inner(handle, host, slot, &exec_log_path, results, batch).await;

    // Drop the worker-side per-batch ktest-tmp dir (scratch devices,
    // sockets, env files). ktest's own EXIT trap usually cleans it
    // up, but doesn't fire on SIGKILL / parent-reaped — that's the
    // mechanism the /tmp/ktest-* leaks were going through. Always
    // ssh+rm here so the daemon is the source of truth for that dir.
    let ws = format!("ktest-ci/{}", slot);
    let _ = handle
        .run_command(ssh_cmd(host, &format!("rm -rf {}/ktest-tmp", ws), false))
        .await;

    let p = &batch[0].payload;
    let commit_dir = p.output_dir.join(&p.commit);

    // Fallback: any subtest that didn't get a full_log.br from inner
    // (inner exited Err before its prepend ran) gets a header-only
    // full_log written here so the dashboard has something. The
    // executor-log slice from batch start to now captures whatever
    // ssh output we saw.
    let missing: Vec<&ClaimedJob<JobParams>> = batch
        .iter()
        .filter(|j| {
            let key = subtest_result_key(
                &j.payload.test,
                &j.payload.subtest,
                &j.payload.kernel,
                &j.payload.env,
            );
            !commit_dir.join(&key).join("full_log.br").exists()
        })
        .collect();

    if !missing.is_empty() {
        let content = format_full_log(
            host,
            slot,
            p,
            &batch
                .iter()
                .map(|j| j.payload.subtest.as_str())
                .collect::<Vec<_>>(),
            &exec_log_path,
            exec_log_offset_start,
            &[], // no supervisor body — inner didn't reach the pull
        );
        for j in missing {
            let key = subtest_result_key(
                &j.payload.test,
                &j.payload.subtest,
                &j.payload.kernel,
                &j.payload.env,
            );
            let d = commit_dir.join(&key);
            let full_log_path = d.join("full_log");
            if let Err(e) =
                std::fs::create_dir_all(&d).and_then(|()| std::fs::write(&full_log_path, &content))
            {
                handle.log_line(format!(
                    "fallback full_log {}: {}",
                    full_log_path.display(),
                    e
                ));
                continue;
            }
            if let Err(e) = brotli_compress(&full_log_path) {
                handle.log_line(format!("brotli {}: {}", full_log_path.display(), e));
            }
        }
    }

    // On any failure that left subtests marked IN PROGRESS, write a
    // FailedToRun verdict for them. The daemon failed to launch / pull
    // for this subtest in this batch — that IS a verdict (the infra
    // step is the test being measured for this purpose). Leaving them
    // IN PROGRESS would stick them forever (job_wanted skips a live
    // Inprogress); deleting them would re-emit on every refill until a
    // VM finally landed, which buries a systematically-broken subtest.
    if result.is_err() {
        let now = Utc::now();
        let mut updates = TestResultsMap::new();
        for j in batch {
            let key = subtest_result_key(
                &j.payload.test,
                &j.payload.subtest,
                &j.payload.kernel,
                &j.payload.env,
            );
            if results.lookup(&p.commit, &key) == Some(TestStatus::Inprogress) {
                let d = commit_dir.join(&key);
                let _ = std::fs::create_dir_all(&d)
                    .and_then(|()| std::fs::write(d.join("status"), "FAILED TO RUN\n"));
                updates.insert(
                    key,
                    TestResult {
                        status: TestStatus::FailedToRun,
                        starttime: now,
                        duration: 0,
                    },
                );
            }
        }
        results.update(&p.commit, updates);
    }

    result
}

async fn run_ktest_job_inner(
    handle: &ExecutorHandle<JobParams>,
    host: &str,
    slot: usize,
    exec_log_path: &std::path::Path,
    results: &TestResultsStore,
    batch: &[ClaimedJob<JobParams>],
) -> Result<(), TaskError> {
    let p = &batch[0].payload;
    if p.repo_url.is_empty() {
        return Err(TaskError::Fatal(format!("repo {} not configured", p.repo)));
    }
    let ws = format!("ktest-ci/{}", slot);
    let basename = result_basename(&p.test, &p.kernel, &p.env);
    let commit_dir = p.output_dir.join(&p.commit);

    handle.log_line(format!("=== batch on {}:{} ===", host, slot));

    // Subtests still owed a verdict — the whole claimed batch to start.
    let mut remaining: Vec<String> = batch.iter().map(|j| j.payload.subtest.clone()).collect();

    // Mark every subtest in-progress immediately: in the store (which
    // is what the cgi reads via the capnp) and in our output_dir
    // (debugging artifact). The worker-side mark is written by the
    // prepare step below so a dead VM still leaves a status. Doing
    // this up front — before checkout — means the cgi shows the
    // claim the moment an executor picks the batch, not after the
    // multi-second checkout + supervisor build.
    let now = Utc::now();
    let mut inprogress_map = TestResultsMap::new();
    for st in &remaining {
        let key = subtest_result_key(&p.test, st, &p.kernel, &p.env);
        let d = commit_dir.join(&key);
        if let Err(e) = std::fs::create_dir_all(&d)
            .and_then(|()| std::fs::write(d.join("status"), "IN PROGRESS\n"))
        {
            handle.log_line(format!("marking {st} in-progress: {e}"));
        }
        inprogress_map.insert(
            key,
            TestResult {
                status: TestStatus::Inprogress,
                starttime: now,
                duration: 0,
            },
        );
    }
    results.update(&p.commit, inprogress_map);

    // 1. Check out the repo at the commit. A repo switch (different
    //    repo, or none) wipes the workspace — the stale checkout and
    //    its kernel build cache are both invalid.
    // Each slot has its own git repo (no concurrent legitimate user),
    // so a leftover .git/index.lock is from a previous batch whose git
    // op got SIGKILL'd between O_EXCL-creating the lock and releasing
    // it. Drop it before fetch.
    // The git:// fetch/clone from the worker is occasionally flaky (transient
    // network resets); a single failed fetch shouldn't fail the whole batch's
    // checkout. Wrap the network ops in a shell retry-with-backoff.
    let checkout = format!(
        "set -e; \
         retry() {{ n=0; until \"$@\"; do n=$((n+1)); [ $n -ge 5 ] && return 1; echo \"checkout: retry $n: $*\" >&2; sleep $((n*n)); done; }}; \
         if [ ! -d {ws}/{repo}/.git ]; then \
             rm -rf {ws}; mkdir -p {ws}; \
             retry git -C {ws} clone {url} {repo}; \
         fi; \
         cd {ws}/{repo}; \
         rm -f .git/index.lock; \
         retry git fetch {url} {commit}; \
         git checkout -f FETCH_HEAD; \
         test \"$(git rev-parse HEAD)\" = \"{commit}\"",
        ws = ws, repo = p.repo, url = p.repo_url, commit = p.commit,
    );
    run_step(handle, host, &checkout, "checkout").await?;

    // 2. Build the supervisor (idempotent C helper).
    run_step(
        handle,
        host,
        "make -C ~/ktest/lib supervisor",
        "build supervisor",
    )
    .await?;

    // 3. Clean ktest-out (keep the kernel build cache); mark every
    //    subtest in-progress worker-side too so a dead VM still leaves
    //    a status for the supervisor to scan. Once: the resume loop
    //    re-runs only not-completed subtests and must not wipe the
    //    rest's results.
    let mark = remaining
        .iter()
        .map(|st| {
            let d = subtest_result_key(&p.test, st, &p.kernel, &p.env);
            format!("mkdir -p ktest-out/out/{d}; echo 'IN PROGRESS' > ktest-out/out/{d}/status")
        })
        .collect::<Vec<_>>()
        .join("; ");
    let prepare = format!(
        "set -e; cd {ws}; \
         if [ -d ktest-out ]; then \
             find ktest-out -mindepth 1 -maxdepth 1 ! -name 'kernel*' -exec rm -rf {{}} +; \
         fi; \
         {mark}",
        ws = ws,
        mark = mark,
    );
    run_step(handle, host, &prepare, "prepare").await?;

    // Resume loop: run the not-yet-completed subtests in one VM; retry
    // any the VM died before reaching.
    while !remaining.is_empty() {
        // Mark this iter's window in the executor log so the post-pull
        // full_log write splices in exactly what we did this iter.
        let iter_offset = handle.log_offset();

        // 4. Run the supervisor over `remaining`, one VM. Its exit
        //    status is ignored — verdicts are in the result files; only
        //    a failure to *run* it is an error.
        handle.log_line(format!("=== run: {} ===", remaining.join(" ")));
        // -T <tmp>: caller-managed VM working dir (scratch devices,
        // sockets, env files). Resolves under $ws (CWD of the inner
        // shell is $ws after the leading 'cd {ws};') so the per-batch
        // teardown below catches it even if bash's EXIT trap doesn't
        // fire (SIGKILL, OOM); avoids /tmp/ktest-* leaks.
        let runner = if p.kernel.is_empty() {
            format!(
                "~/ktest/build-test-kernel run -k {}/{} -T ktest-tmp -P",
                ws, p.repo
            )
        } else {
            format!("~/ktest/ktest run -k {} -T ktest-tmp", p.kernel)
        };
        // The supervisor's -f full-log is one file per VM run; it lives
        // in remaining[0]'s dir, and every other subtest's full_log.br
        // is linked to it after the pull.
        let full_log = format!(
            "{}/full_log",
            subtest_result_key(&p.test, &remaining[0], &p.kernel, &p.env),
        );
        let inner = format!(
            "cd {ws}; {env}~/ktest/lib/supervisor -T 1200 -f {full_log} \
             -S -F -b {base} -o ktest-out/out -- {runner} ~/ktest/tests/{test} {subtests}",
            ws = ws,
            env = job_env_prefix(&p.env, &ws),
            full_log = full_log,
            base = basename,
            runner = runner,
            test = p.test,
            subtests = remaining.join(" "),
        );
        // build-test-kernel compiles the kernel on the worker — it needs
        // ci/shell.nix's toolchain. `ktest run` builds inside the VM.
        let run = if p.kernel.is_empty() {
            format!("nix-shell ~/ktest/ci/shell.nix --run \"{}\"", inner)
        } else {
            inner
        };
        // Stopgap: a build interrupted mid-write leaves a corrupt cached
        // object the incremental rebuild then trusts — git-clean and try
        // once more. Each {run} does its own cd, so subshell it.
        let run = format!(
            "( {run} ) || {{ echo '=== run failed -- git clean + retry ==='; \
             git -C ~/{ws}/{repo} clean -fdqx; ( {run} ); }}",
            run = run,
            ws = ws,
            repo = p.repo,
        );
        // -tt: force a pty so a dropped ssh hangs up and SIGHUP reaps
        // the supervisor, the kernel build, and the VM together.
        handle
            .run_command(ssh_cmd(host, &run, true))
            .await
            .map_err(|e| TaskError::Retry(format!("running supervisor: {e}")))?;

        // 5. Pull the remaining subtests' result dirs back to the
        //    daemon's output_dir.
        handle.log_line("=== pull results ===".to_string());
        let dirs = remaining
            .iter()
            .map(|st| subtest_result_key(&p.test, st, &p.kernel, &p.env))
            .collect::<Vec<_>>()
            .join(" ");
        let pull = format!(
            "mkdir -p {dst} && ssh {opts} {host} 'cd {ws}/ktest-out/out && tar -c {dirs}' \
             | tar -x -C {dst}",
            dst = commit_dir.display(),
            opts = SSH_OPTS.join(" "),
            host = host,
            ws = ws,
            dirs = dirs,
        );
        let mut pull_cmd = Command::new("bash");
        pull_cmd.arg("-c").arg(&pull);
        let status = handle
            .run_command(pull_cmd)
            .await
            .map_err(|e| TaskError::Retry(format!("pulling results: {e}")))?;
        if !status.success() {
            return Err(TaskError::Retry(format!(
                "pulling results: exited {:?}",
                status.code()
            )));
        }

        // The cgi serves logs as .br. Drop any full_log.br a prior
        // iteration linked into these subtests' dirs — a retried
        // subtest relinks to the iter that actually produced its
        // verdict.
        for st in &remaining {
            let _ = std::fs::remove_file(
                commit_dir
                    .join(subtest_result_key(&p.test, st, &p.kernel, &p.env))
                    .join("full_log.br"),
            );
        }

        // Compose this iter's full_log: batch header + iter slice of
        // executor log + supervisor body (the pulled file). Write to
        // remaining[0]'s dir as the canonical, brotli, then symlink
        // every other subtest's full_log.br to it.
        let primary_key = subtest_result_key(&p.test, &remaining[0], &p.kernel, &p.env);
        let primary_dir = commit_dir.join(&primary_key);
        let primary_path = primary_dir.join("full_log");
        let supervisor_body = std::fs::read(&primary_path).unwrap_or_default();
        let content = format_full_log(
            host,
            slot,
            p,
            &remaining.iter().map(String::as_str).collect::<Vec<_>>(),
            exec_log_path,
            iter_offset,
            &supervisor_body,
        );
        if let Err(e) = std::fs::write(&primary_path, &content) {
            handle.log_line(format!("write full_log {}: {}", primary_path.display(), e));
        } else if let Err(e) = brotli_compress(&primary_path) {
            handle.log_line(format!("brotli {}: {}", primary_path.display(), e));
        }

        // Brotli per-subtest "log" files (one per test the supervisor
        // reached).
        for st in &remaining {
            let d = commit_dir.join(subtest_result_key(&p.test, st, &p.kernel, &p.env));
            let path = d.join("log");
            if path.exists() {
                if let Err(e) = brotli_compress(&path) {
                    handle.log_line(format!("compress {}: {}", path.display(), e));
                }
            }
        }

        // Every other subtest in this iter shares the same VM run —
        // symlink each one's full_log.br to the primary's.
        for st in remaining.iter().skip(1) {
            let link = commit_dir
                .join(subtest_result_key(&p.test, st, &p.kernel, &p.env))
                .join("full_log.br");
            if let Err(e) =
                std::os::unix::fs::symlink(format!("../{primary_key}/full_log.br"), &link)
            {
                handle.log_line(format!("full_log link for {st}: {e}"));
            }
        }

        // 6. The supervisor wrote a status for every subtest it reached
        //    (Passed/Failed, or FAILED TO RUN if it couldn't launch one).
        //    A subtest still IN PROGRESS was never reached — the VM died
        //    first — and is retried in a fresh VM. Read each batch
        //    subtest's pulled status from disk, merge into the store
        //    (one locked rewrite of the capnp), and pick out the ones
        //    that need another VM.
        let mut next = Vec::new();
        let mut verdicts = TestResultsMap::new();
        for st in &remaining {
            let key = subtest_result_key(&p.test, st, &p.kernel, &p.env);
            let dir = commit_dir.join(&key);
            let r = read_test_result(&dir).unwrap_or(TestResult {
                status: TestStatus::Inprogress,
                starttime: now,
                duration: 0,
            });
            if r.status == TestStatus::Inprogress {
                next.push(st.clone());
            }
            verdicts.insert(key, r);
        }
        results.update(&p.commit, verdicts);

        // A run that completed none of the subtests is an infrastructure
        // failure — re-running the same VM the same way won't help.
        if next.len() == remaining.len() {
            return Err(TaskError::Retry(format!(
                "run completed no subtests: {}",
                remaining.join(" ")
            )));
        }
        remaining = next;
    }

    Ok(())
}

/// One executor's body: claim a duration-bounded batch of subtests, run
/// them in one VM, report each job's outcome. Loops until the Choir is
/// dropped. `budget` is subtest_duration_max — the most VM-time of work
/// to pack into a boot.
async fn run_executor(
    mut handle: ExecutorHandle<JobParams>,
    host: String,
    slot: usize,
    results: Arc<TestResultsStore>,
    budget: f64,
) {
    while let Some(batch) = handle.claim(budget).await {
        let outcome = match run_ktest_job(&handle, &host, slot, &results, &batch).await {
            Ok(()) => JobOutcome::Completed,
            Err(e) => JobOutcome::Failed(e.to_string()),
        };
        for j in &batch {
            handle.report(j.id, outcome.clone());
        }
    }
}

/// Build the jobkit JobSpec for a desired job. Subtests of one test
/// file at the same (user, repo, branch, commit, kernel, env) share a
/// batch key, so an executor claims and runs them together in one VM.
fn make_job_spec(rc: &CiConfig, job: &Job) -> JobSpec<JobParams> {
    let k = &job.key;
    let name = format!("{} {} {}", short_commit(&k.commit), k.test, k.subtest);
    let repo_url = rc
        .ktest
        .repo_url(&k.repo)
        .map(String::from)
        .unwrap_or_default();
    let params = JobParams {
        repo: k.repo.clone(),
        commit: k.commit.clone(),
        kernel: k.kernel.clone(),
        env: k.env.clone(),
        test: k.test.clone(),
        subtest: k.subtest.clone(),
        repo_url,
        output_dir: rc.ktest.output_dir.clone(),
    };
    let nice = job.nice + rc.ktest.user_nice.get(&k.user).copied().unwrap_or(0);
    // Mirror the old user_stats_select_fair multiplier: higher nice =
    // more weight = the user is scheduled less often.
    let weight = (1.0 + nice as f64).max(0.1);
    // Batch key: everything but the subtest. `\0` can't occur in any
    // field, so distinct keys can't collide.
    let batch_key = format!(
        "{}\0{}\0{}\0{}\0{}\0{}",
        k.user, k.repo, k.commit, k.kernel, k.env, k.test,
    );
    JobSpec::new(name, params)
        .batch_key(batch_key)
        .cost(job.duration as f64)
        .group(k.user.clone())
        .weight(weight)
}

// --- reconcile + status ---

/// Top the job window back up. Finished jobs are dropped from the choir
/// first — a still-desired one (e.g. a failed infra step) is then free
/// to be re-submitted on this same pass. Then submit the newest desired
/// jobs not already tracked; desired_jobs() caps itself at `window`, so
/// the choir never holds much more than that.
fn refill(
    choir: &Choir<JobParams>,
    job_map: &mut HashMap<JobKey, JobId>,
    rc: &CiConfig,
    results: &TestResultsStore,
    window: usize,
) {
    choir.remove(|_| true);
    let live: HashSet<JobId> = choir.status().jobs.iter().map(|j| j.id).collect();
    job_map.retain(|_, id| live.contains(id));

    let desired = desired_jobs(rc, results, window);
    let mut submitted = 0;
    for job in &desired {
        if job_map.contains_key(&job.key) {
            continue;
        }
        let id = choir.submit(make_job_spec(rc, job));
        job_map.insert(job.key.clone(), id);
        submitted += 1;
    }
    eprintln!(
        "refill: {} desired, {} submitted, {} tracked",
        desired.len(),
        submitted,
        job_map.len(),
    );
}

/// Write the Choir's status snapshot to the file the cgi reads.
/// Written via a temp file + rename so the cgi never sees a partial.
fn write_status(choir: &Choir<JobParams>, rc: &CiConfig) {
    let json = match serde_json::to_string_pretty(&choir.status()) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("status serialize failed: {}", e);
            return;
        }
    };
    let path = rc.ktest.output_dir.join("ci-daemon-status.json");
    let tmp = path.with_extension("json.new");
    if let Err(e) = std::fs::write(&tmp, &json).and_then(|()| std::fs::rename(&tmp, &path)) {
        eprintln!("writing {}: {}", path.display(), e);
    }
}

/// Run a periodic-maintenance binary (gc-results, gen-avg-duration),
/// best-effort — a failure is logged, not fatal.
fn run_maintenance(name: &str) {
    match std::process::Command::new(name).status() {
        Ok(s) if s.success() => eprintln!("maintenance: {} ok", name),
        Ok(s) => eprintln!("maintenance: {} exited {:?}", name, s.code()),
        Err(e) => eprintln!("maintenance: {} failed to run: {}", name, e),
    }
}

/// Open one ssh master connection per worker host. The executors' many
/// per-step ssh calls then multiplex over it (ControlMaster=auto in
/// SSH_OPTS); without this, 50 executors each open a fresh connection
/// at once and storm the workers' sshd MaxStartups.
fn prewarm_ssh_masters(rc: &CiConfig) {
    use std::process::Stdio;
    // Spawn every master connection at once, then collect them — a
    // serial pre-warm would cost one full `timeout` (30s) per down host.
    // `timeout` caps a wedged ssh so it can't hang startup; stdio is
    // detached so the persisted master doesn't hold the daemon's
    // streams open.
    let spawned: Vec<_> = rc
        .ktest
        .executors
        .keys()
        .filter_map(|host| {
            let child = std::process::Command::new("timeout")
                .arg("30")
                .arg("ssh")
                .args(SSH_OPTS)
                .arg(host)
                .arg("true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match child {
                Ok(c) => Some((host, c)),
                Err(e) => {
                    eprintln!("ci-daemon: ssh master to {}: {}", host, e);
                    None
                }
            }
        })
        .collect();
    for (host, mut child) in spawned {
        match child.wait() {
            Ok(s) if s.success() => eprintln!("ci-daemon: ssh master to {} ready", host),
            Ok(s) => eprintln!(
                "ci-daemon: ssh master to {} failed (exit {:?})",
                host,
                s.code()
            ),
            Err(e) => eprintln!("ci-daemon: ssh master to {}: {}", host, e),
        }
    }
}

/// `git fetch <fetch>` in `path`, then point the local ref
/// `<user>/<branch>` at the freshly-fetched FETCH_HEAD.
fn fetch_branch(path: &std::path::Path, user: &str, branch: &str, fetch: &str) -> Result<()> {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("fetch")
        .args(fetch.split_whitespace())
        .status()?;
    if !status.success() {
        anyhow::bail!("git fetch {} in {}: {}", fetch, path.display(), status);
    }

    let repo = git2::Repository::open(path)?;
    let fetch_head = repo.revparse_single("FETCH_HEAD")?.peel_to_commit()?;
    repo.branch(&format!("{}/{}", user, branch), &fetch_head, true)?;
    Ok(())
}

/// Background thread: keep the CI's git refs current. `refill` / jobs.rs
/// build the job matrix as a pure read of the per-(user,branch) local
/// refs `<user>/<branch>`, so without this nothing new is picked up.
///
/// Ported from the old gen-job-list `fetch_remotes`: per branch, run the
/// configured `git fetch <remote> <ref>` — targeted, never `--all`, so CI
/// bookkeeping tags (`origin/master_<date>` etc.) can't trip refname
/// conflicts — then point `<user>/<branch>` at FETCH_HEAD.
fn spawn_repo_fetcher(rc: &CiConfig) {
    // Snapshot (user, branch, repo path, fetch args); the thread outlives `rc`.
    let mut branches: Vec<(String, String, std::path::PathBuf, String)> = Vec::new();
    for (user, userconfig) in &rc.users {
        let Ok(userconfig) = userconfig else { continue };
        for (branch, bc) in &userconfig.branches {
            match rc.ktest.repo_path(&bc.repo) {
                Some(path) => branches.push((
                    user.clone(),
                    branch.clone(),
                    path.to_path_buf(),
                    bc.fetch.clone(),
                )),
                None => eprintln!("ci-daemon: repo-fetcher: no path for repo {}", bc.repo),
            }
        }
    }

    std::thread::spawn(move || loop {
        for (user, branch, path, fetch) in &branches {
            if let Err(e) = fetch_branch(path, user, branch, fetch) {
                eprintln!("ci-daemon: fetch {}/{}: {}", user, branch, e);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    });
}

fn main() -> Result<()> {
    let args = Args::parse();
    eprintln!("ci-daemon: starting");
    let rc = ciconfig_read()?;
    eprintln!("ci-daemon: config loaded");

    // Load the test result store. IN PROGRESS entries from a previous
    // daemon are stale (no longer running) and dropped — desired_jobs()
    // then re-emits those subtests. A live IN PROGRESS, written by an
    // executor in this run, never makes it back to disk-load: it's
    // either updated to a verdict before the next restart or cleared on
    // run_ktest_job error.
    eprintln!(
        "ci-daemon: loading test results from {}",
        rc.ktest.output_dir.display()
    );
    let load_start = std::time::Instant::now();
    let results = Arc::new(TestResultsStore::load(
        rc.ktest.output_dir.clone(),
        args.clear_failed_to_run,
    ));
    eprintln!(
        "ci-daemon: test results loaded in {:.1}s",
        load_start.elapsed().as_secs_f64()
    );

    let choir: Choir<JobParams> = Choir::new(rc.ktest.output_dir.join("ci-daemon-logs"));

    // Pre-open an ssh master per host so the executors' per-step ssh
    // calls multiplex over it instead of storming sshd MaxStartups.
    eprintln!(
        "ci-daemon: prewarming ssh masters to {} hosts",
        rc.ktest.executors.len()
    );
    prewarm_ssh_masters(&rc);
    eprintln!("ci-daemon: ssh master prewarm complete");

    // One executor per slot. The body claims a duration-bounded batch
    // of one test file's subtests and runs them in a single VM.
    let budget = rc.ktest.subtest_duration_max.unwrap_or(600) as f64;
    for (host, ex) in &rc.ktest.executors {
        for slot in 0..ex.slots {
            let cfg = ExecutorConfig {
                name: format!("{}:{}", host, slot),
            };
            let host = host.clone();
            let slot = slot as usize;
            let results = Arc::clone(&results);
            choir.add_executor(cfg, move |_cfg, handle| {
                run_executor(handle, host, slot, results, budget)
            });
        }
    }
    let total_slots: u32 = rc.ktest.executors.values().map(|e| e.slots).sum();
    eprintln!(
        "ci-daemon: {} hosts, {} executors",
        rc.ktest.executors.len(),
        total_slots,
    );

    // Keep the CI repos current; the job matrix is a pure read of local refs.
    spawn_repo_fetcher(&rc);

    let mut job_map: HashMap<JobKey, JobId> = HashMap::new();
    let window = args.limit.unwrap_or(WINDOW);
    let mut last_maintenance: Option<std::time::Instant> = None;

    loop {
        refill(&choir, &mut job_map, &rc, &results, window);
        write_status(&choir, &rc);

        if args.once {
            choir.join_all();
            write_status(&choir, &rc);
            return Ok(());
        }

        // Periodic upkeep the old ci-loop used to drive.
        if last_maintenance.map_or(true, |t| t.elapsed() > MAINTENANCE_INTERVAL) {
            run_maintenance("gc-results");
            run_maintenance("gen-avg-duration");
            last_maintenance = Some(std::time::Instant::now());
        }

        // Rewrite the status snapshot every STATUS_INTERVAL; refill once
        // the window has drained low enough to want topping up.
        loop {
            std::thread::sleep(STATUS_INTERVAL);
            write_status(&choir, &rc);
            let pending: usize = choir.status().pending_by_group.values().sum();
            if pending <= window / 4 {
                break;
            }
        }
    }
}
