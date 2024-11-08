use std::path::PathBuf;
use clap::Parser;
use serde_derive::Deserialize;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    id:         String,
    #[arg(short, long)]
    idx:        Option<usize>,
    #[arg(short, long)]
    output:     PathBuf,
    #[arg(short, long)]
    verbose:    bool,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct SyzCrash {
    title:                  String,
    #[serde(rename = "syz-reproducer")]
    syz_reproducer:         Option<String>,
    #[serde(rename = "c-reproducer")]
    c_reproducer:           Option<String>,
    #[serde(rename = "kernel-config")]
    kernel_config:          String,
    #[serde(rename = "kernel-source-git")]
    kernel_source_git:      String,
    #[serde(rename = "kernel-source-commit")]
    kernel_source_commit:   String,
    #[serde(rename = "syzkaller-git")]
    syzkaller_git:          String,
    #[serde(rename = "syzkaller-commit")]
    syzkaller_commit:       String,
    #[serde(rename = "crash-report-link")]
    crash_report_link:      String,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct SyzBug {
    version:        usize,
    title:          String,
    id:             String,
    #[serde(default)]
    discussions:    Vec<String>,
    crashes:        Vec<SyzCrash>,
}

fn get_syz_url(url: &str) -> String {
    let url = format!("https://syzkaller.appspot.com{}", url);
    reqwest::blocking::get(url).unwrap().text().unwrap()
}

fn fetch_syz_url(args: &Args, url: &str, fname: &str) {
    let fname = args.output.join(fname);
    if !fname.exists() {
        if args.verbose { eprintln!("fetching {} => {:?}", &url, &fname); }

        std::fs::write(fname, get_syz_url(url)).ok();
    } else {
        if args.verbose { eprintln!("have {} => {:?}", &url, &fname); }
    }
}

fn fetch_syz_crash(args: &Args, crash: &SyzCrash, idx: usize) {
    fetch_syz_url(&args, &crash.kernel_config, &format!("{}.{}.kconfig", args.id, idx));

    if let Some(r) = &crash.c_reproducer.as_ref() {
        fetch_syz_url(&args, r, &format!("{}.{}.repro.c", args.id, idx));
    }
}

fn main() {
    let args = Args::parse();

    let bug_json = get_syz_url(&format!("/bug?json=1&extid={}", args.id));
    let bug: SyzBug = serde_json::from_str(&bug_json).unwrap();

    if args.verbose { eprintln!("{:?}", &bug); }

    std::fs::create_dir_all(&args.output).ok();

    if let Some(idx) = args.idx {
        fetch_syz_crash(&args, &bug.crashes[idx], idx);
    } else {
        for (idx, crash) in bug.crashes.iter().enumerate() {
            fetch_syz_crash(&args, &crash, idx);
        }
    }
}
