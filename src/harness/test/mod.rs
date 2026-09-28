pub(crate) mod agent_loop_test;
pub(crate) mod bash_dispatch;
pub(crate) mod bug_hunt;
pub(crate) mod chat_tests;
pub(crate) mod command_mode;
pub(crate) mod compaction_wedge;
pub(crate) mod dispatch;
// pub(crate) mod find_dispatch;
#[cfg(feature = "onnx")]
pub(crate) mod checkup_onnx;
pub(crate) mod internal_tool;
pub(crate) mod loop_latency;
pub(crate) mod manual_compaction;
pub(crate) mod permission;
pub(crate) mod reasoning_ownership;
pub(crate) mod repro_compaction_loop;
