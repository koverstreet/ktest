// ci-daemon: push-mode CI job runner.
//
// Runs on the jobserver. Builds a jobkit Choir with one executor per
// configured worker host (each running up to `slots` jobs at once),
// then loops:
//   * compute the desired job set (ci_cgi::jobs::desired_jobs)
//   * reconcile against the in-memory job table — submit new, cancel +
//     drop stale (force-pushed, done)
//   * write a status snapshot for the cgi
//
// Each job is one subtest. The executor closure ssh's the worker host
// through checkout → prepare → build supervisor → run → pull results;
// "what is running" is the in-memory table, not lockfiles.
//
// Deferred: gcov/lcov upload and log compression (peripheral to the
// core test-running path); the daemon's own git fetch (the old CI's
// fetch keeps the branch refs fresh during the transition).

use anyhow::{anyhow, Result};
use ci_cgi::jobs::{desired_jobs, Job, JobKey};
use ci_cgi::{ciconfig_read, commit_update_results, result_basename, CiConfig};
use clap::Parser;
use jobkit::{Choir, Command, ExecutorConfig, JobContext, JobId, JobSpec, TaskError};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

/// How often to recompute the desired set and reconcile.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// How often the status snapshot is rewritten, so the cgi's live page
/// stays fresh between reconciles.
const STATUS_INTERVAL: Duration = Duration::from_secs(2);

/// Reconcile ticks between periodic-maintenance runs — gc-results and
/// gen-avg-duration, the upkeep the old ci-loop used to drive.
const GC_EVERY: u64 = 10;
const DURATIONS_EVERY: u64 = 60;

/// ssh options: connection multiplexing so the daemon's many per-step
/// ssh calls to one worker share a single connection. BatchMode so a
/// missing key fails fast instead of hanging on a prompt.
const SSH_OPTS: &[&str] = &[
    "-o", "ControlMaster=auto",
    "-o", "ControlPath=~/.ssh/ci-master-%r@%h:%p",
    "-o", "ControlPersist=10m",
    "-o", "ServerAliveInterval=10",
    "-o", "BatchMode=yes",
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
    /// `git fetch` URL the worker pulls the repo from (`ci_host:path`);
    /// empty if the repo isn't configured.
    repo_url: String,
    /// Daemon-local results dir; pulled results land in `<it>/<commit>/`.
    output_dir: PathBuf,
}

/// An ssh command to `host` running `remote` (one shell command line).
fn ssh_cmd(host: &str, remote: &str) -> Command {
    let mut c = Command::new("ssh");
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
async fn run_step(ctx: &JobContext, host: &str, remote: &str, desc: &str) -> Result<(), TaskError> {
    ctx.log_line(format!("=== {} ===", desc));
    let status = ctx
        .run_command(ssh_cmd(host, remote))
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

/// Run one subtest job on `ctx`'s executor: ssh the worker host through
/// checkout → prepare → build supervisor → run → pull results.
///
/// Returns Err only on infrastructure failure (a step couldn't run). A
/// test that runs and fails is a normal Ok — its verdict is in the
/// result files, pulled back in the last step.
async fn run_ktest_job(ctx: JobContext, p: JobParams) -> Result<(), TaskError> {
    if p.repo_url.is_empty() {
        return Err(TaskError::Fatal(format!("repo {} not configured", p.repo)));
    }
    let host = ctx.executor().host.clone();
    let ws = format!("ktest-ci/{}", ctx.slot());
    let basename = result_basename(&p.test, &p.kernel, &p.env);
    let result_dir = format!("ktest-out/out/{}.{}", basename, p.subtest.replace('/', "."));

    // 1. Check out the repo at the commit. A repo switch (the workspace
    //    has a different repo, or none) wipes the workspace — the stale
    //    checkout and its kernel build cache are both invalid.
    let checkout = format!(
        "set -e; \
         if [ ! -d {ws}/{repo}/.git ]; then \
             rm -rf {ws}; mkdir -p {ws}; \
             git -C {ws} clone {url} {repo}; \
         fi; \
         cd {ws}/{repo}; \
         git fetch {url} {commit}; \
         git checkout -f FETCH_HEAD; \
         test \"$(git rev-parse HEAD)\" = \"{commit}\"",
        ws = ws, repo = p.repo, url = p.repo_url, commit = p.commit,
    );
    run_step(&ctx, &host, &checkout, "checkout").await?;

    // 2. Clean ktest-out (keep the kernel build cache), and mark the
    //    subtest in-progress so a dead VM still leaves a status.
    let prepare = format!(
        "set -e; cd {ws}; \
         if [ -d ktest-out ]; then \
             find ktest-out -mindepth 1 -maxdepth 1 ! -name 'kernel*' -exec rm -rf {{}} +; \
         fi; \
         mkdir -p {rd}; \
         echo 'IN PROGRESS' > {rd}/status",
        ws = ws, rd = result_dir,
    );
    run_step(&ctx, &host, &prepare, "prepare").await?;

    // 3. Build the supervisor (idempotent C helper).
    run_step(&ctx, &host, "make -C ~/ktest/lib supervisor", "build supervisor").await?;

    // 4. Run the test. The supervisor's exit status reflects test
    //    pass/fail — recorded in the result files — so only a failure
    //    to *run* it is an error; the status itself is ignored.
    ctx.log_line("=== run ===".to_string());
    let runner = if p.kernel.is_empty() {
        format!("~/ktest/build-test-kernel run -k {}/{} -P", ws, p.repo)
    } else {
        format!("~/ktest/ktest run -k {}", p.kernel)
    };
    let inner = format!(
        "cd {ws}; {env}~/ktest/lib/supervisor -T 1200 -f {rd}/full_log \
         -S -F -b {base} -o ktest-out/out -- {runner} ~/ktest/tests/{test} {subtest}",
        ws = ws,
        env = job_env_prefix(&p.env, &ws),
        rd = result_dir,
        base = basename,
        runner = runner,
        test = p.test,
        subtest = p.subtest,
    );
    // build-test-kernel compiles the kernel on the worker — it needs
    // ci/shell.nix's toolchain (wrapped rustc + rust-src, bindgen,
    // libclang). `ktest run` builds inside the VM, needing none of it.
    // `inner` has no embedded double-quotes, so the --run wrap nests
    // cleanly.
    let run = if p.kernel.is_empty() {
        format!("nix-shell ~/ktest/ci/shell.nix --run \"{}\"", inner)
    } else {
        inner
    };
    ctx.run_command(ssh_cmd(&host, &run))
        .await
        .map_err(|e| TaskError::Retry(format!("running supervisor: {e}")))?;

    // 5. Pull the subtest's result dir back to the daemon's output_dir.
    ctx.log_line("=== pull results ===".to_string());
    let commit_dir = p.output_dir.join(&p.commit);
    let pull = format!(
        "mkdir -p {dst} && ssh {opts} {host} 'cd {ws}/ktest-out/out && tar -c .' \
         | tar -x -C {dst}",
        dst = commit_dir.display(),
        opts = SSH_OPTS.join(" "),
        host = host,
        ws = ws,
    );
    let mut pull_cmd = Command::new("bash");
    pull_cmd.arg("-c").arg(&pull);
    let status = ctx
        .run_command(pull_cmd)
        .await
        .map_err(|e| TaskError::Retry(format!("pulling results: {e}")))?;
    if !status.success() {
        return Err(TaskError::Retry(format!(
            "pulling results: exited {:?}",
            status.code()
        )));
    }

    // Compress the test logs the cgi serves as .br (the worker writes
    // them plain). Best-effort — a missing log is not a job failure.
    ctx.log_line("=== compress logs ===".to_string());
    let pulled = commit_dir.join(format!("{}.{}", basename, p.subtest.replace('/', ".")));
    for log in ["log", "full_log"] {
        let path = pulled.join(log);
        if path.exists() {
            if let Err(e) = brotli_compress(&path) {
                ctx.log_line(format!("compress {}: {}", path.display(), e));
            }
        }
    }

    // 6. Regenerate the commit's results capnp so the next reconcile
    //    tick sees the new result and stops desiring this job.
    commit_update_results(&p.output_dir, &p.commit);

    Ok(())
}

/// Build the jobkit JobSpec for a desired job.
fn make_job_spec(ci_host: &str, rc: &CiConfig, job: &Job) -> JobSpec {
    let k = &job.key;
    let name = format!("{} {} {}", short_commit(&k.commit), k.test, k.subtest);
    let repo_url = rc
        .ktest
        .repo_path(&k.repo)
        .map(|path| format!("{}:{}", ci_host, path.display()))
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
    let nice = rc.ktest.user_nice.get(&k.user).copied().unwrap_or(0);
    // Mirror the old user_stats_select_fair multiplier: higher nice =
    // more weight = the user is scheduled less often.
    let weight = (1.0 + nice as f64).max(0.1);
    JobSpec::new(name, move |ctx| {
        let params = params.clone();
        async move { run_ktest_job(ctx, params).await }
    })
    .group(k.user.clone())
    .weight(weight)
}

// --- reconcile + status ---

/// Reconcile the desired job set against the in-memory job table.
fn reconcile(
    choir: &Choir,
    job_map: &mut HashMap<JobKey, JobId>,
    ci_host: &str,
    rc: &CiConfig,
    limit: Option<usize>,
    desired: &[Job],
) {
    let desired_keys: HashSet<&JobKey> = desired.iter().map(|j| &j.key).collect();

    // Submit jobs wanted but not yet tracked, up to --limit.
    let mut submitted = 0;
    for job in desired {
        if job_map.contains_key(&job.key) {
            continue;
        }
        if limit.map_or(false, |n| job_map.len() >= n) {
            break;
        }
        let id = choir.submit(make_job_spec(ci_host, rc, job));
        job_map.insert(job.key.clone(), id);
        submitted += 1;
    }

    // Cancel jobs no longer wanted (force-pushed away, or now done);
    // remove drops the ones that have actually finished.
    let stale: HashSet<JobId> = job_map
        .iter()
        .filter(|(key, _)| !desired_keys.contains(*key))
        .map(|(_, id)| *id)
        .collect();
    if !stale.is_empty() {
        choir.cancel(|j| stale.contains(&j.id));
        choir.remove(|j| stale.contains(&j.id));
    }

    // Drop map entries for jobs jobkit has removed from the table, so a
    // key that becomes wanted again later is re-submitted.
    let live: HashSet<JobId> = choir.status().jobs.iter().map(|j| j.id).collect();
    job_map.retain(|_, id| live.contains(id));

    eprintln!(
        "reconcile: {} desired, {} submitted, {} stale, {} tracked",
        desired.len(),
        submitted,
        stale.len(),
        job_map.len(),
    );
}

/// Write the Choir's status snapshot to the file the cgi reads.
/// Written via a temp file + rename so the cgi never sees a partial.
fn write_status(choir: &Choir, rc: &CiConfig) {
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

fn main() -> Result<()> {
    let args = Args::parse();
    let rc = ciconfig_read()?;

    let ci_host = rc
        .ktest
        .ci_host
        .clone()
        .ok_or_else(|| anyhow!("ci_host not set in ~/.ktest/ktest-ci.json5"))?;

    let choir = Choir::new(rc.ktest.output_dir.join("ci-daemon-logs"));

    // One executor per worker host, running up to `slots` jobs at once.
    for (host, ex) in &rc.ktest.executors {
        choir.add_executor(ExecutorConfig {
            name: host.clone(),
            host: host.clone(),
            capacity: ex.slots as usize,
        });
    }
    let total_slots: u32 = rc.ktest.executors.values().map(|e| e.slots).sum();
    eprintln!(
        "ci-daemon: {} executors, {} total slots",
        rc.ktest.executors.len(),
        total_slots,
    );

    let mut job_map: HashMap<JobKey, JobId> = HashMap::new();
    let mut tick = 0u64;

    loop {
        reconcile(&choir, &mut job_map, &ci_host, &rc, args.limit, &desired_jobs(&rc));
        write_status(&choir, &rc);

        if args.once {
            choir.join_all();
            write_status(&choir, &rc);
            return Ok(());
        }

        // Periodic maintenance the old ci-loop used to drive.
        if tick % GC_EVERY == 0 {
            run_maintenance("gc-results");
        }
        if tick % DURATIONS_EVERY == 0 {
            run_maintenance("gen-avg-duration");
        }
        tick += 1;

        // Rewrite the status snapshot every STATUS_INTERVAL so the cgi's
        // live page stays current; reconcile only every RECONCILE_INTERVAL.
        let next_reconcile = std::time::Instant::now() + RECONCILE_INTERVAL;
        while std::time::Instant::now() < next_reconcile {
            std::thread::sleep(STATUS_INTERVAL);
            write_status(&choir, &rc);
        }
    }
}
