use ci_cgi::{commit_capnp_set_message, ktestrc_read};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Migrate existing capnp files to include commit messages")]
struct Args {
    /// Show what would be done without writing
    #[arg(long)]
    dry_run: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let rc = ktestrc_read()?;

    unsafe {
        git2::opts::set_verify_owner_validation(false)
            .expect("set_verify_owner_validation should never fail");
    }

    let repo = git2::Repository::open(&rc.linux_repo)?;

    let pattern = rc.output_dir.join("*.capnp").to_string_lossy().to_string();
    let mut updated = 0;
    let mut skipped = 0;
    let mut errors = 0;

    for entry in glob::glob(&pattern)? {
        let path = entry?;
        let stem = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // Skip non-commit files (workers, user_stats, test_durations, branch.*)
        if stem.len() < 20 || !stem.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }

        let commit_id = stem.to_string();

        // Look up commit message from git
        let message = match repo.revparse_single(&commit_id) {
            Ok(obj) => match obj.peel_to_commit() {
                Ok(commit) => commit.message().unwrap_or("").to_string(),
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            },
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let subject = message.lines().next().unwrap_or("");

        if args.dry_run {
            eprintln!("would update: {} {}", &commit_id[..12], subject);
        } else {
            match commit_capnp_set_message(&rc.output_dir, &commit_id, &message) {
                Ok(()) => eprintln!("updated: {} {}", &commit_id[..12], subject),
                Err(e) => {
                    eprintln!("error updating {}: {}", &commit_id[..12], e);
                    errors += 1;
                    continue;
                }
            }
        }
        updated += 1;
    }

    eprintln!();
    if args.dry_run {
        eprintln!("dry run: {} would be updated, {} skipped (not in git), {} errors",
            updated, skipped, errors);
    } else {
        eprintln!("{} updated, {} skipped (not in git), {} errors",
            updated, skipped, errors);
    }

    Ok(())
}
