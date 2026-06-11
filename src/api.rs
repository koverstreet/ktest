//! Wire types shared by the dashboard cgi (which serializes them) and
//! ci-status (which deserializes them): the JSON contract that lets
//! ci-status query the CI server directly instead of needing a local
//! clone of every repo and a mirror of the results directory.
//!
//! These shapes intentionally match ci-status's existing --json output,
//! which predates them; both sides now use these structs so the formats
//! can't drift.

use serde::{Deserialize, Serialize};

// The branch view (`?user=X&branch=Y&format=json`) serializes
// crate::BranchEntry — one struct, shared with the local capnp path, so
// the two can't drift.

/// One test's result within a commit
/// (`?user=X&branch=Y&commit=Z&format=json`).
#[derive(Debug, Serialize, Deserialize)]
pub struct TestEntry {
    pub name: String,
    /// TestStatus::to_str() form ("PASSED", "FAILED", ...)
    pub status: String,
    pub duration: u64,
}

/// Per-test detail for one commit.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitTests {
    pub commit: String,
    pub message: String,
    pub tests: Vec<TestEntry>,
}

/// A user's configured branches (`?user=X&format=json`, and one element
/// of the `?format=json` index).
#[derive(Debug, Serialize, Deserialize)]
pub struct UserBranches {
    pub user: String,
    pub branches: Vec<String>,
}

/// JSON-mode error body; non-2xx responses carry this.
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
