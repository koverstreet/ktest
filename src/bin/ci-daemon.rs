// ci-daemon: push-mode CI job runner.
//
// Runs on the jobserver. Builds a jobkit Choir with one executor per
// worker-host slot, then keeps a bounded *window* of jobs submitted:
//   * desired_jobs() yields the newest commits' jobs first, capped
//   * submit that window; as it drains, regenerate and top it back up
//   * write a status snapshot for the cgi
//
// The matrix can be millions of jobs; jobkit only ever holds the
// window (~2000). Each job is one subtest — the executor closure ssh's
// the worker through checkout → prepare → build supervisor → run →
// pull results.
//
// Deferred: gcov/lcov upload; the daemon's own git fetch and reconcile
// on branch change (it currently refills only as the window drains).

use anyhow::{anyhow, Result};
use ci_cgi::jobs::{desired_jobs, Job, JobKey};
use ci_cgi::{ciconfig_read, commit_update_results, result_basename, CiConfig};
use clap::Parser;
use jobkit::{Choir, Command, ExecutorConfig, JobContext, JobId, JobSpec, TaskError};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
    "-o", "ControlMaster=auto",
    "-o", "ControlPath=~/.ssh/ci-master-%r@%h:%p",
    "-o", "ControlPersist=10m",
    "-o", "ServerAliveInterval=10",
    "-o", "ConnectTimeout=20",
    "-o", "BatchMode=yes",
    // Trust a worker's host key on first contact (the farm is ours);
    // BatchMode can't prompt, so an unknown host would otherwise fail.
    "-o", "StrictHostKeyChecking=accept-new",
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
async fn run_step(ctx: &JobContext, host: &str, remote: &str, desc: &str) -> Result<(), TaskError> {
    ctx.log_line(format!("=== {} ===", desc));
    let status = ctx
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
    let subtest_dir = format!("{}.{}", basename, p.subtest.replace('/', "."));
    let result_dir = format!("ktest-out/out/{}", subtest_dir);

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
        "cd {ws}; {env}~/ktest/lib/supervisor -T 1200 -f {sub}/full_log \
         -S -F -b {base} -o ktest-out/out -- {runner} ~/ktest/tests/{test} {subtest}",
        ws = ws,
        env = job_env_prefix(&p.env, &ws),
        sub = subtest_dir,
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
    // Stopgap: if the run fails, git-clean the workspace and try once
    // more -- a build interrupted mid-write leaves a corrupt cached
    // object that the incremental rebuild then trusts. Also self-heals
    // workspaces already poisoned.
    // Each {run} does its own `cd`, so run it in a subshell — otherwise
    // the first run's cd leaks and the git -C / retry resolve wrong.
    let run = format!(
        "( {run} ) || {{ echo '=== run failed -- git clean + retry ==='; \
         git -C ~/{ws}/{repo} clean -fdqx; ( {run} ); }}",
        run = run, ws = ws, repo = p.repo,
    );
    // -tt: force a pty so a dropped ssh hangs up and SIGHUP reaps the
    // supervisor, the kernel build, and the VM together.
    ctx.run_command(ssh_cmd(&host, &run, true))
        .await
        .map_err(|e| TaskError::Retry(format!("running supervisor: {e}")))?;

    // 5. Pull *only* this job's subtest dir back to the daemon's
    //    output_dir. Pulling all of ktest-out/out would drag back the
    //    supervisor's whole-test-file in-progress markers and clobber
    //    every other subtest's real result.
    ctx.log_line("=== pull results ===".to_string());
    let commit_dir = p.output_dir.join(&p.commit);
    let pull = format!(
        "mkdir -p {dst} && ssh {opts} {host} 'cd {ws}/ktest-out/out && tar -c {sub}' \
         | tar -x -C {dst}",
        dst = commit_dir.display(),
        opts = SSH_OPTS.join(" "),
        host = host,
        ws = ws,
        sub = subtest_dir,
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
    let pulled = commit_dir.join(&subtest_dir);
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

/// Top the job window back up. Finished jobs are dropped from the choir
/// first — a still-desired one (e.g. a failed infra step) is then free
/// to be re-submitted on this same pass. Then submit the newest desired
/// jobs not already tracked; desired_jobs() caps itself at `window`, so
/// the choir never holds much more than that.
fn refill(
    choir: &Choir,
    job_map: &mut HashMap<JobKey, JobId>,
    ci_host: &str,
    rc: &CiConfig,
    window: usize,
) {
    choir.remove(|_| true);
    let live: HashSet<JobId> = choir.status().jobs.iter().map(|j| j.id).collect();
    job_map.retain(|_, id| live.contains(id));

    let desired = desired_jobs(rc, window);
    let mut submitted = 0;
    for job in &desired {
        if job_map.contains_key(&job.key) {
            continue;
        }
        let id = choir.submit(make_job_spec(ci_host, rc, job));
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
            Ok(s) => eprintln!("ci-daemon: ssh master to {} failed (exit {:?})", host, s.code()),
            Err(e) => eprintln!("ci-daemon: ssh master to {}: {}", host, e),
        }
    }
}

/// `git fetch <fetch>` in `path`, then point the local ref
/// `<user>/<branch>` at the freshly-fetched FETCH_HEAD.
fn fetch_branch(
    path: &std::path::Path,
    user: &str,
    branch: &str,
    fetch: &str,
) -> Result<()> {
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
                Some(path) =>
                    branches.push((user.clone(), branch.clone(), path.to_path_buf(), bc.fetch.clone())),
                None =>
                    eprintln!("ci-daemon: repo-fetcher: no path for repo {}", bc.repo),
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
    let rc = ciconfig_read()?;

    let ci_host = rc
        .ktest
        .ci_host
        .clone()
        .ok_or_else(|| anyhow!("ci_host not set in ~/.ktest/ktest-ci.json5"))?;

    let choir = Choir::new(rc.ktest.output_dir.join("ci-daemon-logs"));

    // Pre-open an ssh master per host so the executors' per-step ssh
    // calls multiplex over it instead of storming sshd MaxStartups.
    prewarm_ssh_masters(&rc);

    // One executor per slot: each host gets `slots` executors.
    for (host, ex) in &rc.ktest.executors {
        for slot in 0..ex.slots {
            choir.add_executor(ExecutorConfig {
                name: format!("{}:{}", host, slot),
                host: host.clone(),
                slot: slot as usize,
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
        refill(&choir, &mut job_map, &ci_host, &rc, window);
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
