extern crate libc;
use ci_cgi::{ciconfig_read, git_get_commit, CiConfig};
use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::DirEntry;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
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

/// The commits whose results stay: each within keep_results_commits of a
/// branch tip, by its distance from the nearest; and of those, the ones the
/// CI still tests - within the max_commits of a branch's test groups - whose
/// results would only be run again.
#[derive(Default)]
struct Live {
    depth:  HashMap<String, usize>,
    tested: HashSet<String>,
}

fn get_live_commits(rc: &CiConfig) -> Live {
    let mut ret = Live::default();
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

            let tested = branch_config.test_groups.iter()
                .filter_map(|g| userconfig.test_groups.get(g))
                .map(|g| g.max_commits as usize)
                .max()
                .unwrap_or(0);

            let userbranch = format!("{}/{}", user, branch);
            for (i, commit) in branch_get_commits(&repo, &userbranch, depth).into_iter().enumerate() {
                if i < tested {
                    ret.tested.insert(commit.clone());
                }
                let d = ret.depth.entry(commit).or_insert(i);
                *d = (*d).min(i);
            }
        }
    }

    ret
}

fn result_is_live(live: &Live, d: &DirEntry, touch: bool) -> bool {
    let fname = d.file_name().into_string().ok();

    if let Some(fname) = fname {
        /* If it's not actually a commit, don't delete it: */
        if fname.len() < 40 {
            return true;
        }

        if live.depth.contains_key(&fname[..40]) {
            // Not in a dry run: the age check reads mtimes, so touching
            // them would change what the next run removes
            if touch {
                if let Ok(f) = std::fs::File::open(d.path()) {
                    let _ = f.set_modified(std::time::SystemTime::now());
                }
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

    let live = get_live_commits(&rc);
    let now = std::time::SystemTime::now();
    let older_than = now.checked_sub(std::time::Duration::new(3600, 0)).unwrap();

    for d in rc
        .ktest
        .output_dir
        .read_dir()
        .unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| !result_is_live(&live, &d, !args.dry_run))
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

    if let Some(max) = rc.ktest.results_max_used_percent {
        shrink_to(&rc.ktest.output_dir, &live, max, args.dry_run);
    }
}

/// df's Use%: used, over used and what's available to an unprivileged
/// user, rounded up.
fn used_percent(path: &Path) -> Option<u64> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let used = (s.f_blocks - s.f_bfree) as u128;
    let total = used + s.f_bavail as u128;
    (total != 0).then(|| (used * 100).div_ceil(total) as u64)
}

/// Remove live results, oldest first - farthest from the nearest branch tip
/// - until @dir's filesystem is no more than @max percent used: never those
/// the CI still tests. Not by age, as the pass above: it has just touched
/// every live result.
fn shrink_to(dir: &Path, live: &Live, max: u64, dry_run: bool) {
    let Some(used) = used_percent(dir) else {
        eprintln!("statvfs {}: {}", dir.display(), std::io::Error::last_os_error());
        return;
    };
    if used <= max {
        return;
    }

    let mut oldest: Vec<(&String, usize)> = live.depth.iter()
        .filter(|(commit, _)| !live.tested.contains(*commit))
        .map(|(commit, depth)| (commit, *depth))
        .collect();
    oldest.sort_by(|a, b| b.1.cmp(&a.1));

    // A dry run lists every candidate: it can't see where removing would stop
    println!("{} is {}% used, over {}%: removing results, oldest first", dir.display(), used, max);

    for (commit, depth) in oldest {
        let result = dir.join(commit);
        let summary = dir.join(format!("{commit}.capnp"));
        if !result.exists() && !summary.exists() {
            continue;
        }

        if !dry_run && used_percent(dir).is_some_and(|u| u <= max) {
            return;
        }
        println!("Removing: {} ({} commits from a branch tip)", result.display(), depth);

        if !dry_run {
            std::fs::remove_dir_all(&result).ok();
            std::fs::remove_file(&summary).ok();
        }
    }

    if !dry_run {
        if let Some(u) = used_percent(dir).filter(|u| *u > max) {
            eprintln!("{} still {}% used: what's left is results the CI still tests", dir.display(), u);
        }
    }
}
