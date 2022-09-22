use std::fs::File;
use std::io::{self, BufRead};
use std::error::Error;
use std::path::{Path, PathBuf};
use serde_derive::Deserialize;
use toml;

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

#[derive(Deserialize)]
pub struct Ktestrc {
    pub ci_linux_repo:       PathBuf,
    pub ci_output_dir:       PathBuf,
    pub ci_branches_to_test: PathBuf,
}

pub fn ktestrc_read() -> Result<Ktestrc, Box<dyn Error>> {
    let config = std::fs::read_to_string("/etc/ktest-ci.toml")?;
    let ktestrc: Ktestrc = toml::from_str(&config)?;

    Ok(ktestrc)
}
