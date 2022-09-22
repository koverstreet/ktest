use std::fs::File;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
extern crate dirs;

pub fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where P: AsRef<Path>, {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

pub fn git_get_commit(repo: &git2::Repository, reference: String) -> Result<git2::Commit, git2::Error> {
    let r = repo.revparse_single(&reference);
    if let Err(e) = r {
        eprintln!("Error from resolve_reference_from_short_name {} in {}: {}", reference, repo.path().display(), e);
        return Err(e);
    }

    let r = r.unwrap().peel_to_commit();
    if let Err(e) = r {
        eprintln!("Error from peel_to_commit {} in {}: {}", reference, repo.path().display(), e);
        return Err(e);
    }
    r
}

pub struct Ktestrc {
    pub ci_linux_repo:       PathBuf,
    pub ci_output_dir:       PathBuf,
    pub ci_branches_to_test: PathBuf,
}

pub fn ktestrc_read() -> Ktestrc {
    let mut ktestrc = Ktestrc {
        ci_linux_repo:          PathBuf::new(),
        ci_output_dir:          PathBuf::new(),
        ci_branches_to_test:    PathBuf::new(),
    };

    if let Some(home) = dirs::home_dir() {
        ktestrc.ci_branches_to_test = home.join("BRANCHES-TO-TEST");
    }

    fn ktestrc_get(rc: &'static str, var: &'static str) -> Option<PathBuf> {
        let cmd = format!(". {}; echo -n ${}", rc, var);

        let output = std::process::Command::new("/usr/bin/env")
            .arg("bash")
            .arg("-c")
            .arg(&cmd)
            .output()
            .expect("failed to execute process /usr/bin/env bash");

        if !output.stderr.is_empty() {
            eprintln!("Error executing {}: {}", cmd, String::from_utf8_lossy(&output.stderr));
            std::process::exit(1);
        }

        let output = output.stdout;
        let output = String::from_utf8_lossy(&output);
        let output = output.trim();

        if !output.is_empty() {
            Some(PathBuf::from(output))
        } else {
            None
        }
    }

    if let Some(v) = ktestrc_get("/etc/ktestrc", "JOBSERVER_LINUX_DIR") {
        ktestrc.ci_linux_repo = v;
    }

    if let Some(v) = ktestrc_get("/etc/ktestrc", "JOBSERVER_OUTPUT_DIR") {
        ktestrc.ci_output_dir = v;
    }

    if let Some(v) = ktestrc_get("/etc/ktestrc", "JOBSERVER_BRANCHES_TO_TEST") {
        ktestrc.ci_branches_to_test = v;
    }

    if let Some(v) = ktestrc_get("$HOME/.ktestrc", "JOBSERVER_LINUX_DIR") {
        ktestrc.ci_linux_repo = v;
    }

    if let Some(v) = ktestrc_get("$HOME/.ktestrc", "JOBSERVER_OUTPUT_DIR") {
        ktestrc.ci_output_dir = v;
    }

    if let Some(v) = ktestrc_get("$HOME/.ktestrc", "JOBSERVER_BRANCHES_TO_TEST") {
        ktestrc.ci_branches_to_test = v;
    }

    ktestrc
}
