use ci_cgi::{ktestrc_read, update_lcov};
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

    update_lcov(&ktestrc, &args.commit);
}
