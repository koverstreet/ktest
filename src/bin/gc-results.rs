extern crate libc;
use ci_cgi::{ciconfig_read, git_get_commit, CiConfig};
use clap::Parser;
use std::collections::HashSet;
use std::fs::DirEntry;
use std::process;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    dry_run: bool,
}

fn branch_get_commits(repo: &git2::Repository, branch: &str, max_commits: u64) -> Vec<String> {
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

fn get_live_commits(rc: &CiConfig) -> HashSet<String> {
    let mut ret: HashSet<String> = HashSet::new();
    let depth = rc.ktest.keep_results_commits.unwrap_or(500);

    for (user, userconfig) in rc
        .users
        .iter()
        .filter(|u| u.1.is_ok())
        .map(|(user, userconfig)| (user, userconfig.as_ref().unwrap()))
    {
        for (branch, branch_config) in userconfig.branches.iter() {
            // Walk the branch's own repo: branches can name a repo
            // other than linux (e.g. bcachefs-tools), and result dirs
            // are keyed by that repo's commit hashes. Opening linux_repo
            // unconditionally finds stale same-named refs and yields the
            // wrong commit set — every result then reads as not-live.
            let path = match rc.ktest.repo_path(&branch_config.repo) {
                Some(p) => p,
                None => {
                    eprintln!("no path configured for repo {}", branch_config.repo);
                    continue;
                }
            };
            let repo = match git2::Repository::open(path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error opening {:?}: {}", path, e);
                    continue;
                }
            };

            let userbranch = format!("{}/{}", user, branch);
            for commit in branch_get_commits(&repo, &userbranch, depth) {
                ret.insert(commit);
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

    for d in rc
        .ktest
        .output_dir
        .read_dir()
        .unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| !result_is_live(&commits, &d))
        .map(|d| d.path())
        .filter(|d| d.metadata().unwrap().modified().unwrap() < older_than)
    {
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
