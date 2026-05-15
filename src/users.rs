use anyhow;
use serde_derive::Deserialize;
use std::collections::BTreeMap;
use std::fs::read_to_string;
use std::path::PathBuf;
use toml;

#[derive(Deserialize)]
pub struct RcTestGroup {
    #[serde(default)]
    pub max_commits: u64,
    pub nice: u64,
    #[serde(default)]
    pub test_duration_nice: u64,
    #[serde(default)]
    pub test_always_passes_nice: u64,
    pub tests: Vec<PathBuf>,
}

impl Default for RcTestGroup {
    fn default() -> Self {
        RcTestGroup {
            max_commits: 50,
            nice: 0,
            test_duration_nice: 180,
            test_always_passes_nice: 10,
            tests: Vec::new(),
        }
    }
}

fn default_repo() -> String { "linux".to_string() }

#[derive(Deserialize)]
pub struct RcBranch {
    pub fetch: String,
    pub tests: Vec<String>,
    /// Source repo name; the worker fetches from `$JOBSERVER:~/$repo`
    /// and runs the test from a checkout of that tree. Defaults to
    /// "linux" — the legacy build-test-kernel behavior.
    #[serde(default = "default_repo")]
    pub repo: String,
    /// Kernel-store identifiers to run the tests under
    /// (e.g. "debian/forky", "upstream/stable-kasan"). Each kernel
    /// fans out a separate job per (commit × test × subtest). Empty
    /// (the default) means "build the kernel from `repo` at `commit`",
    /// i.e. the legacy build-test-kernel behavior.
    #[serde(default)]
    pub kernel: Vec<String>,
}

#[derive(Deserialize)]
pub struct Userrc {
    pub test_group: BTreeMap<String, RcTestGroup>,
    pub branch: BTreeMap<String, RcBranch>,
}

pub fn userrc_read(path: &PathBuf) -> anyhow::Result<Userrc> {
    let config = read_to_string(path)?;
    let rc: Userrc = toml::from_str(&config)?;

    Ok(rc)
}
