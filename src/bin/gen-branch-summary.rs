use ci_cgi::{ciconfig_read, generate_branch_log, write_branch_log};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Generate branch log summary capnp files")]
struct Args {
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    branch: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let rc = ciconfig_read()?;

    unsafe {
        git2::opts::set_verify_owner_validation(false)
            .expect("set_verify_owner_validation should never fail");
    }

    let repo = git2::Repository::open(&rc.ktest.linux_repo)?;

    let users: Vec<(String, Vec<String>)> = if let Some(user) = &args.user {
        let userrc = rc.users.get(user)
            .ok_or_else(|| anyhow::anyhow!("user {} not found", user))?;

        let branches = match userrc {
            Ok(u) => {
                if let Some(branch) = &args.branch {
                    vec![branch.clone()]
                } else {
                    u.branch.keys().cloned().collect()
                }
            }
            Err(e) => {
                eprintln!("error reading config for user {}: {}", user, e);
                return Ok(());
            }
        };
        vec![(user.clone(), branches)]
    } else {
        rc.users.iter().filter_map(|(user, userrc)| {
            match userrc {
                Ok(u) => Some((user.clone(), u.branch.keys().cloned().collect())),
                Err(e) => {
                    eprintln!("error reading config for user {}: {}", user, e);
                    None
                }
            }
        }).collect()
    };

    for (user, branches) in &users {
        for branch in branches {
            match generate_branch_log(&repo, &rc.ktest, user, branch) {
                Ok(entries) => {
                    if let Err(e) = write_branch_log(&rc.ktest.output_dir, user, branch, &entries) {
                        eprintln!("error writing branch log for {}/{}: {}", user, branch, e);
                    } else {
                        eprintln!("generated {}/{}: {} commits", user, branch, entries.len());
                    }
                }
                Err(e) => {
                    eprintln!("error generating branch log for {}/{}: {}", user, branch, e);
                }
            }
        }
    }

    Ok(())
}
