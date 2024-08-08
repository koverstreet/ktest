use std::fs::File;
use memmap::MmapOptions;
use std::process;
use ci_cgi::{ciconfig_read, test_duration};
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    test:       String,
    subtest:    String,
}

fn main() {
    let args = Args::parse();

    let rc = ciconfig_read();
    if let Err(e) = rc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let rc = rc.unwrap();
    let rc = rc.ktest;

    let durations_file = File::open(rc.output_dir.join("test_durations.capnp")).ok();
    let durations_map = durations_file.map(|x| unsafe { MmapOptions::new().map(&x).ok() } ).flatten();
    let durations = durations_map.as_ref().map(|x| x.as_ref());

    println!("{:?}", test_duration(durations, &args.test, &args.subtest));
}
