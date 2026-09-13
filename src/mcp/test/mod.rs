//! Scoped unit tests for the MCP client module.
//!
//! One file per scope (mirroring `harness/test`): `auth` covers credential
//! parsing/resolution, `bridge_*` covers protocol
//! conversions, `config_*` covers parsing and validation, `era` stays whole
//! as a short correlated scope, and `manager_*` splits the session manager
//! by concern over a shared duplex-test harness in [`manager_support`].
//! Scopes too small for their own file (under ~50 lines) live here, under
//! section headers: resource/prompt catalog views and footer snapshot
//! counting.

pub(crate) mod auth;
pub(crate) mod bridge_templates;
pub(crate) mod bridge_tools;
pub(crate) mod config_validation;
pub(crate) mod config_wizard;
pub(crate) mod era;
pub(crate) mod manager_catalog;
pub(crate) mod manager_era;
pub(crate) mod manager_lifecycle;
pub(crate) mod manager_support;
pub(crate) mod manager_tools;

// ── Resource and prompt catalog views (bridge) ─────────────────────

use rmcp::model::{Prompt, Resource, ResourceTemplate};

use super::bridge::{prompt_to_info, resource_to_info, template_to_info};

#[test]
fn resource_info_marks_templates() {
    let resource = Resource::new("file:///notes.txt", "notes");
    let info = resource_to_info(&resource);
    assert!(!info.is_template);

    let template = ResourceTemplate::new("file:///docs/{id}", "doc-by-id");
    let tinfo = template_to_info(&template);
    assert!(tinfo.is_template);
    assert_eq!(tinfo.uri, "file:///docs/{id}");

    // Prompt `required: None` defaults to false.
    let prompt = Prompt::new("greet", Some("Greets"), None);
    let pinfo = prompt_to_info(&prompt);
    assert!(pinfo.arguments.is_empty());
}

// ── Footer snapshot counting (types) ───────────────────────────────

use super::types::{ServerSnapshot, ServerStatus, failed_count, ready_count};

#[test]
fn counts_split_ready_and_failed() {
    let snapshots = vec![
        ServerSnapshot::ready("a".to_string(), 2),
        ServerSnapshot::failed("b".to_string(), "boom".to_string()),
        ServerSnapshot {
            name: "c".to_string(),
            status: ServerStatus::Disabled,
            tool_count: 0,
            last_error: None,
        },
    ];
    assert_eq!(ready_count(&snapshots), 1);
    assert_eq!(failed_count(&snapshots), 1);
}
