use anyhow::{anyhow, Context};
use serde_derive::Deserialize;
use std::collections::BTreeMap;
use std::fs::read_to_string;
use std::path::{Path, PathBuf};

/// On-disk schema. All fields except those structurally required to
/// look up a parent are optional; `resolve_group` walks `extends`
/// chains and fills in defaults.
#[derive(Deserialize)]
struct RawTestGroup {
    #[serde(default)]
    extends: Option<String>,
    #[serde(default)]
    max_commits: Option<u64>,
    #[serde(default)]
    nice: Option<u64>,
    #[serde(default)]
    test_duration_nice: Option<u64>,
    #[serde(default)]
    test_always_passes_nice: Option<u64>,
    #[serde(default)]
    tests: Option<Vec<PathBuf>>,
    #[serde(default)]
    kernels: Option<Vec<String>>,
    #[serde(default)]
    env: Option<BTreeMap<String, String>>,
}

fn default_repo() -> String {
    "linux".to_string()
}

#[derive(Deserialize)]
struct RawBranch {
    fetch: String,
    #[serde(default = "default_repo")]
    repo: String,
    test_groups: Vec<String>,
}

#[derive(Deserialize)]
struct RawUserrc {
    test_groups: BTreeMap<String, RawTestGroup>,
    branches: BTreeMap<String, RawBranch>,
}

/// Resolved test group: extends chain flattened, kernels list final,
/// env merged top-down. `kernels` empty means "build the kernel from
/// `repo` at `commit`" (legacy build-test-kernel behavior).
pub struct RcTestGroup {
    pub max_commits: u64,
    pub nice: u64,
    pub test_duration_nice: u64,
    pub test_always_passes_nice: u64,
    pub tests: Vec<PathBuf>,
    pub kernels: Vec<String>,
    pub env: BTreeMap<String, String>,
}

pub struct RcBranch {
    pub fetch: String,
    pub repo: String,
    pub test_groups: Vec<String>,
}

pub struct Userrc {
    pub test_groups: BTreeMap<String, RcTestGroup>,
    pub branches: BTreeMap<String, RcBranch>,
}

fn resolve_group(
    name: &str,
    raw: &BTreeMap<String, RawTestGroup>,
    resolved: &mut BTreeMap<String, RcTestGroup>,
    stack: &mut Vec<String>,
) -> anyhow::Result<()> {
    if resolved.contains_key(name) {
        return Ok(());
    }
    if stack.iter().any(|s| s == name) {
        return Err(anyhow!(
            "cycle in test_group extends: {} -> {}",
            stack.join(" -> "),
            name
        ));
    }

    let g = raw
        .get(name)
        .ok_or_else(|| anyhow!("test_group {:?} not defined", name))?;

    stack.push(name.to_string());

    if let Some(p) = &g.extends {
        resolve_group(p, raw, resolved, stack)?;
    }
    let parent: Option<&RcTestGroup> = g.extends.as_deref().and_then(|p| resolved.get(p));

    let resolved_group = RcTestGroup {
        max_commits: g
            .max_commits
            .or(parent.map(|p| p.max_commits))
            .unwrap_or(50),
        nice: g.nice.or(parent.map(|p| p.nice)).unwrap_or(0),
        test_duration_nice: g
            .test_duration_nice
            .or(parent.map(|p| p.test_duration_nice))
            .unwrap_or(180),
        test_always_passes_nice: g
            .test_always_passes_nice
            .or(parent.map(|p| p.test_always_passes_nice))
            .unwrap_or(10),
        tests: g
            .tests
            .clone()
            .or_else(|| parent.map(|p| p.tests.clone()))
            .unwrap_or_default(),
        kernels: g
            .kernels
            .clone()
            .or_else(|| parent.map(|p| p.kernels.clone()))
            .unwrap_or_default(),
        env: {
            let mut env = parent.map(|p| p.env.clone()).unwrap_or_default();
            if let Some(e) = &g.env {
                for (k, v) in e {
                    env.insert(k.clone(), v.clone());
                }
            }
            env
        },
    };

    stack.pop();
    resolved.insert(name.to_string(), resolved_group);
    Ok(())
}

pub fn userrc_read_str(s: &str) -> anyhow::Result<Userrc> {
    let raw: RawUserrc = json_five::from_str(s)?;

    let mut resolved: BTreeMap<String, RcTestGroup> = BTreeMap::new();
    let mut stack: Vec<String> = Vec::new();
    for name in raw.test_groups.keys() {
        resolve_group(name, &raw.test_groups, &mut resolved, &mut stack)?;
    }

    for (bname, b) in &raw.branches {
        for tg in &b.test_groups {
            if !resolved.contains_key(tg) {
                return Err(anyhow!(
                    "branch {:?} references undefined test_group {:?}",
                    bname,
                    tg
                ));
            }
        }
    }

    let branches = raw
        .branches
        .into_iter()
        .map(|(name, b)| {
            (
                name,
                RcBranch {
                    fetch: b.fetch,
                    repo: b.repo,
                    test_groups: b.test_groups,
                },
            )
        })
        .collect();

    Ok(Userrc {
        test_groups: resolved,
        branches,
    })
}

pub fn userrc_read(path: &Path) -> anyhow::Result<Userrc> {
    let config = read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    userrc_read_str(&config).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extends_inherits_tests_and_overrides_kernels() {
        let rc = userrc_read_str(
            r#"{
            test_groups: {
                base: {
                    max_commits: 100,
                    nice: 0,
                    tests: ["a.ktest", "b.ktest"],
                    kernels: ["upstream/stable-default"],
                },
                ext: {
                    extends: "base",
                    max_commits: 5,
                    nice: 5,
                    kernels: ["upstream/stable-kasan", "upstream/stable-lockdep"],
                },
            },
            branches: {
                br: { fetch: "linux master", repo: "linux", test_groups: ["base", "ext"] },
            },
        }"#,
        )
        .unwrap();
        let base = &rc.test_groups["base"];
        let ext = &rc.test_groups["ext"];
        assert_eq!(base.kernels, vec!["upstream/stable-default"]);
        assert_eq!(
            ext.tests,
            vec![PathBuf::from("a.ktest"), PathBuf::from("b.ktest")]
        );
        assert_eq!(
            ext.kernels,
            vec!["upstream/stable-kasan", "upstream/stable-lockdep"]
        );
        assert_eq!(ext.max_commits, 5);
        assert_eq!(ext.nice, 5);
    }

    #[test]
    fn env_merges_with_parent() {
        let rc = userrc_read_str(
            r#"{
            test_groups: {
                base: {
                    tests: ["a.ktest"],
                    kernels: ["k"],
                    env: { FOO: "1", BAR: "base" },
                },
                ext: {
                    extends: "base",
                    env: { BAR: "child", BAZ: "2" },
                },
            },
            branches: {},
        }"#,
        )
        .unwrap();
        let ext = &rc.test_groups["ext"];
        assert_eq!(ext.env.get("FOO").map(String::as_str), Some("1"));
        assert_eq!(ext.env.get("BAR").map(String::as_str), Some("child"));
        assert_eq!(ext.env.get("BAZ").map(String::as_str), Some("2"));
    }

    #[test]
    fn cycle_detected() {
        let err = userrc_read_str(
            r#"{
            test_groups: {
                a: { extends: "b" },
                b: { extends: "a" },
            },
            branches: {},
        }"#,
        )
        .err()
        .expect("expected cycle error");
        assert!(err.to_string().contains("cycle"), "got: {}", err);
    }

    #[test]
    fn unknown_test_group_ref_errors() {
        let err = userrc_read_str(
            r#"{
            test_groups: { base: { tests: ["a.ktest"], kernels: ["k"] } },
            branches: { br: { fetch: "x", test_groups: ["base", "nope"] } },
        }"#,
        )
        .err()
        .expect("expected undefined test_group error");
        assert!(err.to_string().contains("nope"), "got: {}", err);
    }
}
