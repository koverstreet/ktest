extern crate libc;
use std::process;
use std::collections::HashSet;
use std::fs::DirEntry;
use ci_cgi::{Ktestrc, ktestrc_read, git_get_commit};
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    dry_run:    bool,
}

fn branch_get_commits(repo: &git2::Repository,
                      branch: &str,
                      max_commits: usize) -> Vec<String> {
    let mut walk = repo.revwalk().unwrap();
    let reference = git_get_commit(&repo, branch.to_string());
    if reference.is_err() {
        eprintln!("branch {} not found", branch);
        return Vec::new();
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        eprintln!("Error walking {}: {}", branch, e);
        return Vec::new();
    }

    walk.filter_map(|i| i.ok())
        .take(max_commits)
        .filter_map(|i| repo.find_commit(i).ok())
        .map(|i| i.id().to_string())
        .collect()
}

fn get_live_commits(rc: &Ktestrc) -> HashSet<String>
{
    let repo = git2::Repository::open(&rc.linux_repo);
    if let Err(e) = repo {
        eprintln!("Error opening {:?}: {}", rc.linux_repo, e);
        eprintln!("Please specify correct linux_repo");
        process::exit(1);
    }
    let repo = repo.unwrap();

    rc.branch.iter()
        .flat_map(move |(branch, branchconfig)| branchconfig.tests.iter()
            .filter_map(|i| rc.test_group.get(i)).map(move |test_group| (branch, test_group)))
        .map(|(branch, test_group)| branch_get_commits(&repo, &branch, test_group.max_commits))
        .flatten()
        .collect()
}

fn result_is_live(commits: &HashSet<String>, d: &DirEntry) -> bool {
    let d = d.file_name().into_string().ok();

    if let Some(d) = d {
        commits.contains(&d[..40].to_string())
    } else {
        false
    }
}

fn main() {
    let args = Args::parse();

    let rc = ktestrc_read();
    if let Err(e) = rc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let rc = rc.unwrap();

    let commits = get_live_commits(&rc);

    for d in rc.output_dir.read_dir().unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| !result_is_live(&commits, &d))
        .map(|d| d.path()) {
        println!("Removing: {}", d.to_string_lossy());

        if !args.dry_run {
            if d.is_dir() {
                std::fs::remove_dir_all(d).ok();
            } else {
                std::fs::remove_file(d).ok();
            }
        }
    }
}
