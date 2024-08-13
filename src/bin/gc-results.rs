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

    for (user, userconfig) in rc.users.iter()
        .filter(|u| u.1.is_ok())
        .map(|(user, userconfig)| (user, userconfig.as_ref().unwrap())) {
        for (branch, branch_config) in userconfig.branch.iter()  {
            for test_group in branch_config.tests.iter() {
                let max_commits = userconfig.test_group.get(test_group).map(|x| x.max_commits).unwrap_or(0);
                let userbranch = user.to_string() + "/" + branch;
                for commit in branch_get_commits(&repo, &userbranch, max_commits) {
                    ret.insert(commit);
                }
            }
        }
    }

    ret
}

fn result_is_live(commits: &HashSet<String>, d: &DirEntry) -> bool {
    let fname = d.file_name().into_string().ok();

    if let Some(fname) = fname {
        /* If it's not actually a commit, don't delete it: */
        if fname.len() < 40 {
            return true;
        }

        if commits.contains(&fname[..40].to_string()) {
            let f = std::fs::File::open(d.path());
            if let Ok(f) = f {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            true
        } else {
            false
        }
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
    let now = std::time::SystemTime::now();
    let older_than = now.checked_sub(std::time::Duration::new(3600, 0)).unwrap();

    for d in rc.ktest.output_dir.read_dir().unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| !result_is_live(&commits, &d))
        .map(|d| d.path())
        .filter(|d| d.metadata().unwrap().modified().unwrap() < older_than) {
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
