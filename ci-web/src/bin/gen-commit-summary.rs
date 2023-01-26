use std::process;
use ci_cgi::{ktestrc_read, commitdir_get_results, TestResults};
use clap::Parser;
use toml;

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

    let results = TestResults { d: commitdir_get_results(&ktestrc, &args.commit) };

    let file_contents = toml::to_string(&results).unwrap();

    let commit_summary_fname = ktestrc.output_dir.join(args.commit + ".toml");
    std::fs::write(commit_summary_fname, file_contents).unwrap();
}
