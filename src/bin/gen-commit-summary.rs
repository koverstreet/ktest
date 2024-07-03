use std::process;
use ci_cgi::{ktestrc_read, commit_update_results_from_fs};
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    commit: String,
}

fn main() {
    let args = Args::parse();

    let ktestrc = ktestrc_read();
    if let Err(e) = ktestrc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let ktestrc = ktestrc.unwrap();

    commit_update_results_from_fs(&ktestrc, &args.commit);
}
