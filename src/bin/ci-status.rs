use ci_cgi::{
    branch_get_results, branchlog_parse, ciconfig_read, commitdir_get_results_full,
    count_status, format_duration, get_queue_stats, ktestrc_read, user_stats_get,
    user_stats_recent, workers_get, BranchEntry, Ktestrc, TestStatus,
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
        #[arg(long, default_value = "bcachefs")]
        user: String,
        #[arg(long, default_value = "bcachefs-testing")]
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

fn fetch_remote(base_url: &str, path: &str) -> anyhow::Result<Vec<u8>> {
    let url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    let resp = reqwest::blocking::get(&url)?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}: {}", resp.status(), url);
    }
    Ok(resp.bytes()?.to_vec())
}

fn cmd_log(
    user: &str,
    branch: &str,
    remote: Option<&str>,
    ktest: Option<&Ktestrc>,
    json: bool,
) -> anyhow::Result<()> {
    let entries: Vec<BranchEntry> = if let Some(base_url) = remote {
        let path = format!("branch.{}.{}.capnp", user, branch);
        let data = fetch_remote(base_url, &path)?;
        branchlog_parse(&data)?
    } else {
        let ktest = ktest.ok_or_else(|| anyhow::anyhow!("no local config and no --remote URL"))?;

        unsafe {
            git2::opts::set_verify_owner_validation(false)
                .expect("set_verify_owner_validation should never fail");
        }

        let repo = git2::Repository::open(&ktest.linux_repo)?;
        let all = regex::Regex::new("").unwrap();
        let results = branch_get_results(&repo, ktest, Some(user), Some(branch), None, &all)
            .map_err(|e| anyhow::anyhow!(e))?;

        results.into_iter()
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
            .collect()
    };

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
        println!("No results for {}/{}", user, branch);
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
    remote: Option<&str>,
    ktest: Option<&Ktestrc>,
    json: bool,
) -> anyhow::Result<()> {
    if let Some(_base_url) = remote {
        // For remote, we'd need the commit capnp to be served
        // For now, just support local
        anyhow::bail!("--remote not yet supported for 'show' (needs per-commit capnp serving)");
    }

    let ktest = ktest.ok_or_else(|| anyhow::anyhow!("no local config and no --remote URL"))?;

    let full = commitdir_get_results_full(ktest, commit)?;

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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Try to read local config; it's optional for remote mode
    let ktest = ktestrc_read().ok();

    let remote = args.remote.as_deref();

    match args.command {
        Command::Log { user, branch } => {
            cmd_log(&user, &branch, remote, ktest.as_ref(), args.json)
        }
        Command::Show { commit } => {
            cmd_show(&commit, remote, ktest.as_ref(), args.json)
        }
        Command::Workers => {
            let ktest = ktest.ok_or_else(|| anyhow::anyhow!("workers requires local config (/etc/ktest-ci.toml)"))?;
            cmd_workers(&ktest, args.json)
        }
        Command::Summary => {
            let ktest = ktest.ok_or_else(|| anyhow::anyhow!("summary requires local config (/etc/ktest-ci.toml)"))?;
            cmd_summary(&ktest, args.json)
        }
    }
}
