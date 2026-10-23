//! Git integration for the app: one scope for every git question the code
//! asks, on the application side of the crate boundary.
//!
//! Two strategies live here on purpose, and the choice between them is a
//! property of the QUESTION, not of the caller:
//!
//! - [`branch`] answers "which branch is this working directory on?" by
//!   reading `.git/HEAD` directly — never shelling out. It runs once per
//!   rendered footer segment (rate-limited), so a subprocess (or the C
//!   dependency libgit2, which breaks on reftable repositories) is not an
//!   option.
//! - [`worktree`] answers "create a git worktree at this path, checking out
//!   this branch" by running the real `git` binary. That operation cannot be
//!   reproduced by reading files, and its refusals ("already checked out at
//!   …", "contains modified or untracked files") are exactly what the user
//!   needs to see, so they surface verbatim.
//!
//! Neither module is a tool: no schema, no `ToolDescription`, no registry
//! entry. The TUI (slash command) and the harness (`agents_worktree` tool)
//! are the only callers. Keeping them here rather than in `crates/cosh-tools`
//! also keeps the git shelling out of the SDK's dependency graph.

pub mod branch;
pub mod worktree;
