extern crate libc;
use std::process;
use std::collections::HashSet;
use std::fs::DirEntry;
use ci_cgi::{CiConfig, ciconfig_read, git_get_commit};
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    dry_run:    bool,
}

fn branch_get_commits(repo: &git2::Repository,
                      branch: &str,
                      max_commits: u64) -> Vec<String> {
    let max_commits = max_commits.try_into().unwrap();
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

fn get_live_commits(rc: &CiConfig) -> HashSet<String>
{
    let repo = git2::Repository::open(&rc.ktest.linux_repo);
    if let Err(e) = repo {
        eprintln!("Error opening {:?}: {}", rc.ktest.linux_repo, e);
        eprintln!("Please specify correct linux_repo");
        process::exit(1);
    }
    let repo = repo.unwrap();

    let mut ret: HashSet<String> = HashSet::new();

    for (_, user) in rc.users.iter() {
        for (branch, branch_config) in user.branch.iter()  {
            for test_group in branch_config.tests.iter() {
                let max_commits = user.test_group.get(test_group).map(|x| x.max_commits).unwrap_or(0);
                for commit in branch_get_commits(&repo, &branch, max_commits) {
                    ret.insert(commit);
                }
            }
        }
    }

    ret
}

fn result_is_live(commits: &HashSet<String>, d: &DirEntry) -> bool {
    let d = d.file_name().into_string().ok();

    if let Some(d) = d {
        /* If it's not actually a commit, don't delete it: */
        if d.len() < 40 {
            return true;
        }

        commits.contains(&d[..40].to_string())
    } else {
        false
    }
}

fn main() {
    let args = Args::parse();

    let rc = ciconfig_read();
    if let Err(e) = rc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let rc = rc.unwrap();

    let commits = get_live_commits(&rc);

    for d in rc.ktest.output_dir.read_dir().unwrap()
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
