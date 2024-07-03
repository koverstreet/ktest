use std::collections::BTreeMap;
use std::fs::read_to_string;
use std::path::PathBuf;
use serde_derive::Deserialize;
use toml;
use anyhow;

#[derive(Deserialize)]
pub struct RcTestGroup {
    pub max_commits:        u64,
    pub priority:           u64,
    pub tests:              Vec<PathBuf>,
}

#[derive(Deserialize)]
pub struct RcBranch {
    pub fetch:              String,
    pub tests:              Vec<String>,
}

#[derive(Deserialize)]
pub struct Userrc {
    pub test_group:         BTreeMap<String, RcTestGroup>,
    pub branch:             BTreeMap<String, RcBranch>,
}

pub fn userrc_read(path: &PathBuf) -> anyhow::Result<Userrc> {
    let config = read_to_string(path)?;
    let rc: Userrc = toml::from_str(&config)?;

    Ok(rc)
}
