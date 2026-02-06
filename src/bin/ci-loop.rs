use chrono::Local;
use clap::Parser;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

#[derive(Parser)]
#[command(
    author,
    version,
    about = "CI loop - periodically updates job lists and cleans results"
)]
struct Args {
    /// Seconds to sleep between iterations
    #[arg(long, default_value = "60")]
    interval: u64,

    /// Run gen-avg-duration every N iterations
    #[arg(long, default_value = "10")]
    duration_interval: u64,
}

fn log(msg: &str) {
    let now = Local::now();
    eprintln!("[{}] {}", now.format("%H:%M:%S"), msg);
}

fn run_command(name: &str) -> bool {
    log(&format!("Running {}...", name));

    let status = Command::new(name).status();

    match status {
        Ok(s) if s.success() => {
            log(&format!("{} completed", name));
            true
        }
        Ok(s) => {
            log(&format!("{} failed with exit code {:?}", name, s.code()));
            false
        }
        Err(e) => {
            log(&format!("{} failed to execute: {}", name, e));
            false
        }
    }
}

fn main() {
    let args = Args::parse();

    log("CI loop starting");
    log(&format!(
        "Settings: interval={}s, duration_interval={}",
        args.interval, args.duration_interval
    ));

    let mut iteration = 0u64;

    loop {
        // Run gen-avg-duration periodically
        if iteration % args.duration_interval == 0 {
            run_command("gen-avg-duration");
        }

        run_command("gen-job-list");
        run_command("gc-results");

        iteration += 1;

        log(&format!("Sleeping {}s...", args.interval));
        sleep(Duration::from_secs(args.interval));
    }
}
