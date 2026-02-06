use anyhow::{anyhow, Context, Result};
use ci_cgi::{ciconfig_read, commit_update_results};
use clap::Parser;
use glob::Pattern;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about = "Delete test results for specific tests")]
struct Args {
    /// Test name pattern (glob syntax). Use * to match subtests.
    /// Examples: "rust/rust-analyzer*", "fs/bcachefs/snapshots*"
    test: String,

    /// Git revision range - if not specified, matches all commits
    #[arg(short = 'c', long)]
    commits: Option<String>,

    /// Repository path (defaults to linux_repo from config)
    #[arg(short, long)]
    repo: Option<PathBuf>,

    /// Output directory containing results (defaults to output_dir from config)
    #[arg(short, long)]
    output_dir: Option<PathBuf>,

    /// Show what would be deleted without deleting
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn get_commits_in_range(repo: &git2::Repository, range: &str) -> Result<HashSet<String>> {
    let commits = if range.contains("..") {
        let revspec = repo.revparse(range)?;
        let from = revspec.from().ok_or_else(|| anyhow!("invalid range: missing 'from'"))?;
        let to = revspec.to().ok_or_else(|| anyhow!("invalid range: missing 'to'"))?;

        let mut walk = repo.revwalk()?;
        walk.push(to.id())?;
        walk.hide(from.id())?;

        walk.filter_map(|oid| oid.ok()).map(|oid| oid.to_string()).collect()
    } else {
        let commit = repo.revparse_single(range)?.peel_to_commit()?;
        vec![commit.id().to_string()].into_iter().collect()
    };
    Ok(commits)
}

fn find_matching_results(
    output_dir: &PathBuf,
    test_pattern: &Pattern,
    commits: Option<&HashSet<String>>,
) -> Result<Vec<PathBuf>> {
    let dominated_by = |name: &str, prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));

    let mut results = Vec::new();

    for entry in output_dir.read_dir().context("reading output dir")?.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip non-commit entries
        if dominated_by(&name_str, &["jobs.", "workers", "user_stats", "test_durations", "fetch"])
            || name_str.ends_with(".lock")
            || name_str.ends_with(".new")
            || name_str.ends_with(".capnp")
            || name_str.len() < 40
        {
            continue;
        }

        // Filter by commit if specified
        if let Some(commits) = commits {
            if !commits.contains(&name_str[..40]) {
                continue;
            }
        }

        // Look for matching test directories within commit dir
        let commit_dir = entry.path();
        if !commit_dir.is_dir() {
            continue;
        }

        for test_entry in commit_dir.read_dir()?.filter_map(|e| e.ok()) {
            let test_name = test_entry.file_name();
            let test_name = test_name.to_string_lossy();

            if test_pattern.matches(&test_name) {
                results.push(test_entry.path());
            }
        }
    }

    Ok(results)
}

fn remove_path(path: &PathBuf) -> Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Test names use dots as separators in storage, allow slashes for convenience
    let pattern_str = args.test.replace('/', ".");
    let test_pattern = Pattern::new(&pattern_str)
        .with_context(|| format!("invalid test pattern '{}'", args.test))?;

    let rc = ciconfig_read().ok();
    let output_dir = args.output_dir.clone()
        .or_else(|| rc.as_ref().map(|r| r.ktest.output_dir.clone()))
        .ok_or_else(|| anyhow!("No output directory. Use --output-dir or configure output_dir."))?;

    let commits = if let Some(ref range) = args.commits {
        let repo_path = args.repo.clone()
            .or_else(|| rc.as_ref().map(|r| r.ktest.linux_repo.clone()))
            .ok_or_else(|| anyhow!("No repository. Use --repo or configure linux_repo."))?;
        let repo = git2::Repository::open(&repo_path)
            .with_context(|| format!("opening repository {:?}", repo_path))?;
        let commits = get_commits_in_range(&repo, range)
            .with_context(|| format!("parsing range '{}'", range))?;

        if args.verbose {
            eprintln!("Filtering to {} commits", commits.len());
        }
        Some(commits)
    } else {
        None
    };

    let results = find_matching_results(&output_dir, &test_pattern, commits.as_ref())?;

    if results.is_empty() {
        eprintln!("No results found matching '{}'", args.test);
        return Ok(());
    }

    eprintln!("{} {} test results:",
        if args.dry_run { "Would delete" } else { "Deleting" },
        results.len());

    let mut affected_commits: HashSet<String> = HashSet::new();

    for path in &results {
        println!("  {}", path.display());
        if !args.dry_run {
            // Track affected commit (parent directory name)
            if let Some(commit_dir) = path.parent() {
                if let Some(commit) = commit_dir.file_name() {
                    affected_commits.insert(commit.to_string_lossy().into_owned());
                }
            }
            if let Err(e) = remove_path(path) {
                eprintln!("  error: {}", e);
            }
        }
    }

    if args.dry_run {
        eprintln!("\nDry run - nothing deleted. Run without -n to delete.");
    } else {
        // Update capnp summaries for affected commits
        eprintln!("Updating {} commit summaries...", affected_commits.len());
        for commit in &affected_commits {
            commit_update_results(&output_dir, commit);
        }
        eprintln!("Deleted {} results. Run gen-job-list to regenerate jobs.", results.len());
    }

    Ok(())
}
