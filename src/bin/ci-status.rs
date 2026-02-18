use ci_cgi::{
    branch_get_results, ciconfig_read, commitdir_get_results_full,
    count_status, format_duration, get_queue_stats, ktestrc_read, Ktestrc, Userrc,
    user_stats_get, user_stats_recent, workers_get, BranchEntry, TestStatus,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "CLI interface for bcachefs CI test results")]
struct Args {
    #[command(subcommand)]
    command: Command,

    /// Read from local output_dir (default if /etc/ktest-ci.toml exists)
    #[arg(long, global = true)]
    local: bool,

    /// Fetch capnp over HTTPS from remote URL
    #[arg(long, global = true)]
    remote: Option<String>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Branch log: commit list with pass/fail counts
    Log {
        /// Git ref — branch name resolved against ci_remote,
        /// or explicit remote/branch (e.g. bcachefs/bcachefs-testing)
        branch: String,
    },
    /// Commit detail: per-test status and duration
    Show {
        /// Commit hash (prefix ok)
        commit: String,
    },
    /// Worker status grouped by host
    Workers,
    /// One-line summary of test counts
    Summary,
    /// List branches from CI user config
    Branches,
    /// Fetch CI user config from server
    PullConfig,
    /// Push CI user config to server
    PushConfig,
}

// ANSI color helpers
fn color_passed(s: &str) -> String   { format!("\x1b[32m{}\x1b[0m", s) }
fn color_failed(s: &str) -> String   { format!("\x1b[31m{}\x1b[0m", s) }
fn color_inprog(s: &str) -> String   { format!("\x1b[33m{}\x1b[0m", s) }
fn color_dim(s: &str) -> String      { format!("\x1b[2m{}\x1b[0m", s) }

fn color_status(status: TestStatus) -> String {
    let s = status.to_str();
    match status {
        TestStatus::Passed     => color_passed(s),
        TestStatus::Failed     => color_failed(s),
        TestStatus::Inprogress => color_inprog(s),
        _                      => color_dim(s),
    }
}

/// Resolve a branch name to a git ref, trying:
/// 1. ci_remote/name (e.g. "bcachefs/bcachefs-testing") — preferred, tracks remote
/// 2. The name as-is (e.g. explicit "bcachefs/bcachefs-testing", "HEAD", a commit hash)
fn resolve_branch(repo: &git2::Repository, ktest: &Ktestrc, name: &str) -> anyhow::Result<String> {
    // Try ci_remote/name first — we want the remote tracking branch, not a stale local
    if let Some(ref remote) = ktest.ci_remote {
        let with_remote = format!("{}/{}", remote, name);
        if repo.revparse_single(&with_remote).is_ok() {
            return Ok(with_remote);
        }
    }

    // Fall back to as-is (explicit ref, commit hash, etc.)
    if repo.revparse_single(name).is_ok() {
        return Ok(name.to_string());
    }

    anyhow::bail!("can't resolve '{}' — try a full ref like 'remote/branch'", name)
}

fn cmd_log(
    branch: &str,
    ktest: &Ktestrc,
    json: bool,
) -> anyhow::Result<()> {
    unsafe {
        git2::opts::set_verify_owner_validation(false)
            .expect("set_verify_owner_validation should never fail");
    }

    let repo = git2::Repository::open(&ktest.linux_repo)?;
    let gitref = resolve_branch(&repo, ktest, branch)?;
    let all = regex::Regex::new("").unwrap();
    let results = branch_get_results(&repo, ktest, None, None, Some(&gitref), &all)
        .map_err(|e| anyhow::anyhow!(e))?;

    let entries: Vec<BranchEntry> = results.into_iter()
        .filter(|r| !r.tests.is_empty())
        .map(|r| {
            let duration: u64 = r.tests.values().map(|t| t.duration).sum();
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
        .collect();

    if json {
        let json_entries: Vec<serde_json::Value> = entries.iter().map(|e| {
            serde_json::json!({
                "commit": &e.commit_id,
                "message": &e.message,
                "passed": e.passed,
                "failed": e.failed,
                "notrun": e.notrun,
                "notstarted": e.notstarted,
                "inprogress": e.inprogress,
                "unknown": e.unknown,
                "duration": e.duration,
            })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&json_entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No results for {}", branch);
        return Ok(());
    }

    // Header
    println!("{:<14} {:>6} {:>6} {:>6} {:>6} {:>8}  {}",
        "COMMIT", "PASS", "FAIL", "NOTST", "INPRO", "DURATION", "MESSAGE");
    println!("{}", "-".repeat(80));

    for e in &entries {
        let subject = e.message.lines().next().unwrap_or("");
        let subject = if subject.len() > 50 { &subject[..50] } else { subject };

        let commit = if e.commit_id.len() >= 12 { &e.commit_id[..12] } else { &e.commit_id };

        let pass_s = format!("{}", e.passed);
        let fail_s = format!("{}", e.failed);

        println!("{:<14} {:>6} {:>6} {:>6} {:>6} {:>8}  {}",
            commit,
            if e.passed > 0 { color_passed(&pass_s) } else { pass_s },
            if e.failed > 0 { color_failed(&fail_s) } else { fail_s },
            e.notstarted,
            e.inprogress,
            format_duration(e.duration),
            subject,
        );
    }

    Ok(())
}

fn cmd_show(
    commit: &str,
    ktest: &Ktestrc,
    json: bool,
) -> anyhow::Result<()> {
    // Resolve short prefix to full hash via git
    let commit = if commit.len() < 40 {
        unsafe {
            git2::opts::set_verify_owner_validation(false)
                .expect("set_verify_owner_validation should never fail");
        }
        let repo = git2::Repository::open(&ktest.linux_repo)?;
        let obj = repo.revparse_single(commit)?;
        obj.id().to_string()
    } else {
        commit.to_string()
    };

    let full = commitdir_get_results_full(ktest, &commit)?;

    if json {
        let json_tests: Vec<serde_json::Value> = full.tests.iter().map(|(name, r)| {
            serde_json::json!({
                "name": name,
                "status": r.status.to_str(),
                "duration": r.duration,
            })
        }).collect();
        let out = serde_json::json!({
            "commit": commit,
            "message": &full.message,
            "tests": json_tests,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if !full.message.is_empty() {
        let subject = full.message.lines().next().unwrap_or("");
        println!("{} {}", commit, subject);
        println!();
    }

    if full.tests.is_empty() {
        println!("No test results for {}", commit);
        return Ok(());
    }

    // Sort tests: failed first, then in-progress, then passed, then rest
    let mut tests: Vec<_> = full.tests.iter().collect();
    tests.sort_by_key(|(_, r)| match r.status {
        TestStatus::Failed     => 0,
        TestStatus::Inprogress => 1,
        TestStatus::Notrun     => 2,
        TestStatus::Unknown    => 3,
        TestStatus::Notstarted => 4,
        TestStatus::Passed     => 5,
    });

    println!("{:<60} {:>12} {:>8}", "TEST", "STATUS", "DURATION");
    println!("{}", "-".repeat(82));

    for (name, result) in &tests {
        println!("{:<60} {:>12} {:>8}",
            name,
            color_status(result.status),
            format_duration(result.duration),
        );
    }

    // Summary line
    let passed = full.tests.values().filter(|r| r.status == TestStatus::Passed).count();
    let failed = full.tests.values().filter(|r| r.status == TestStatus::Failed).count();
    let inprog = full.tests.values().filter(|r| r.status == TestStatus::Inprogress).count();
    let total_duration: u64 = full.tests.values().map(|r| r.duration).sum();

    println!();
    println!("{} total: {} passed, {} failed, {} in progress, {}",
        full.tests.len(),
        color_passed(&passed.to_string()),
        color_failed(&failed.to_string()),
        inprog,
        format_duration(total_duration),
    );

    Ok(())
}

fn cmd_workers(ktest: &Ktestrc, json: bool) -> anyhow::Result<()> {
    let workers = workers_get(ktest)?;

    if json {
        let json_workers: Vec<serde_json::Value> = workers.iter().map(|w| {
            serde_json::json!({
                "hostname": &w.hostname,
                "workdir": &w.workdir,
                "user": &w.user,
                "branch": &w.branch,
                "commit": &w.commit,
                "tests": &w.tests,
                "starttime": w.starttime.to_rfc3339(),
            })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&json_workers)?);
        return Ok(());
    }

    // Group by hostname
    let mut by_host: std::collections::BTreeMap<String, Vec<_>> = std::collections::BTreeMap::new();
    for w in workers {
        by_host.entry(w.hostname.clone()).or_default().push(w);
    }

    let now = chrono::Utc::now();
    let tests_dir = ktest.ktest_dir.to_string_lossy().to_string() + "/tests/";

    for (hostname, host_workers) in &by_host {
        let running = host_workers.iter().filter(|w| !w.tests.is_empty()).count();
        println!("{} ({} workers, {} running)", hostname, host_workers.len(), running);

        for w in host_workers {
            let elapsed = now - w.starttime;
            let elapsed_s = format!("{}:{:02}:{:02}",
                elapsed.num_hours(),
                elapsed.num_minutes() % 60,
                elapsed.num_seconds() % 60);

            let tests = w.tests.strip_prefix(&tests_dir).unwrap_or(&w.tests);

            if w.branch.is_empty() {
                println!("  {:<12} {:>10}  {}",
                    w.workdir,
                    elapsed_s,
                    color_dim("(idle)"),
                );
            } else {
                println!("  {:<12} {:>10}  {:<12} {}",
                    w.workdir,
                    elapsed_s,
                    format!("{}~{}", w.branch, w.age),
                    tests,
                );
            }
        }
        println!();
    }

    Ok(())
}

fn cmd_summary(ktest: &Ktestrc, json: bool) -> anyhow::Result<()> {
    let rc = ciconfig_read()?;
    let queue_stats = get_queue_stats(&rc);

    if json {
        let out = serde_json::json!({
            "pending": queue_stats.total_pending,
            "running": queue_stats.total_running,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{} pending, {} running",
        queue_stats.total_pending,
        queue_stats.total_running,
    );

    // Show per-user breakdown if there's data
    let user_stats = user_stats_get(ktest).unwrap_or_default();
    if !user_stats.is_empty() {
        println!();
        println!("{:<20} {:>8} {:>8} {:>8}", "USER", "PENDING", "RUNNING", "RECENT");
        println!("{}", "-".repeat(48));

        for s in &user_stats {
            let pending = queue_stats.pending_by_user.get(&s.user).unwrap_or(&0);
            let running = queue_stats.running_by_user.get(&s.user).unwrap_or(&0);
            let recent = format_duration(user_stats_recent(s) as u64);

            println!("{:<20} {:>8} {:>8} {:>8}", s.user, pending, running, recent);
        }
    }

    Ok(())
}

fn user_config_path(ktest: &Ktestrc) -> std::path::PathBuf {
    ktest.output_dir.join("ci-user.toml")
}

fn ci_scp_path(ktest: &Ktestrc) -> anyhow::Result<String> {
    let host = ktest.ci_host.as_deref()
        .ok_or_else(|| anyhow::anyhow!("ci_host not set in config"))?;
    Ok(format!("{}:ci.toml", host))
}

fn cmd_pull_config(ktest: &Ktestrc) -> anyhow::Result<()> {
    let remote = ci_scp_path(ktest)?;
    let dest = user_config_path(ktest);
    let _ = std::fs::create_dir_all(&ktest.output_dir);

    let status = std::process::Command::new("scp")
        .arg(&remote)
        .arg(&dest)
        .status()?;

    if !status.success() {
        anyhow::bail!("scp {} failed — you may need to set up ~/ci.toml on the server", remote);
    }

    println!("Config saved to {}", dest.display());
    Ok(())
}

fn cmd_push_config(ktest: &Ktestrc) -> anyhow::Result<()> {
    let remote = ci_scp_path(ktest)?;
    let src = user_config_path(ktest);

    if !src.exists() {
        anyhow::bail!("no local config at {} — run pull-config first", src.display());
    }

    let status = std::process::Command::new("scp")
        .arg(&src)
        .arg(&remote)
        .status()?;

    if !status.success() {
        anyhow::bail!("scp failed");
    }

    println!("Config pushed to {}", remote);
    Ok(())
}

fn cmd_branches(ktest: &Ktestrc, json: bool) -> anyhow::Result<()> {
    let config_path = user_config_path(ktest);
    let config = std::fs::read_to_string(&config_path)
        .map_err(|_| anyhow::anyhow!("no user config — run `ci-status pull-config` first"))?;
    let userrc: Userrc = toml::from_str(&config)?;

    if json {
        let branches: Vec<serde_json::Value> = userrc.branch.iter().map(|(name, b)| {
            serde_json::json!({
                "name": name,
                "fetch": &b.fetch,
                "tests": &b.tests,
            })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&branches)?);
        return Ok(());
    }

    for (name, b) in &userrc.branch {
        let tests = b.tests.join(", ");
        println!("{:<40} {}", name, color_dim(&tests));
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut ktest = ktestrc_read()
        .map_err(|e| anyhow::anyhow!("failed to read /etc/ktest-ci.toml: {}", e))?;

    // --remote overrides ci_url from config
    if let Some(url) = args.remote {
        ktest.ci_url = Some(url);
    }

    match args.command {
        Command::Log { branch } => {
            cmd_log(&branch, &ktest, args.json)
        }
        Command::Show { commit } => {
            cmd_show(&commit, &ktest, args.json)
        }
        Command::Workers => {
            cmd_workers(&ktest, args.json)
        }
        Command::Summary => {
            cmd_summary(&ktest, args.json)
        }
        Command::Branches => {
            cmd_branches(&ktest, args.json)
        }
        Command::PullConfig => {
            cmd_pull_config(&ktest)
        }
        Command::PushConfig => {
            cmd_push_config(&ktest)
        }
    }
}
