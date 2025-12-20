use ci_cgi::{commit_update_results_from_fs, ktestrc_read};
use clap::Parser;
use std::process;

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
