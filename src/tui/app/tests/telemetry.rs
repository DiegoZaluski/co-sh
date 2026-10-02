use super::App;
use cosh::harness::events::{HarnessEvent, LlmCompactionEvent};
use cosh::telemetry::schema::ErrorCategory;
use ratatui::{Terminal, backend::TestBackend};

/// Run real TUI capture points with consent both on and off. Subprocesses
/// isolate environment/config and ensure no developer data is read or queued.
#[tokio::test]
async fn telemetry_tracks_tui_events_and_rendered_views() {
    const NAME: &str = concat!(
        module_path!(),
        "::telemetry_tracks_tui_events_and_rendered_views"
    );
    if std::env::var("COSH_TUI_TELEMETRY_TEST").as_deref() != Ok(NAME) {
        for enabled in ["on", "off"] {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                // libtest names omit the crate prefix included by module_path!().
                .args(["--exact", NAME.split_once("::").unwrap().1, "--nocapture"])
                .env("COSH_TUI_TELEMETRY_TEST", NAME)
                .env("COSH_TELEMETRY", enabled)
                .env_remove("CI")
                // Hermetic-by-construction: a non-https endpoint fails
                // SinkConfig::validate() → flush returns Idle without a single
                // network attempt, EVEN IF this test binary was compiled with
                // a publishable key baked in (option_env! in the sink). The
                // key env var is removed so no runtime override leaks either.
                .env("COSH_TELEMETRY_ENDPOINT", "http://127.0.0.1:9/never")
                .env_remove("COSH_TELEMETRY_PUBLISHABLE_KEY")
                .env("HOME", root.path())
                .env("XDG_DATA_HOME", root.path().join("data"))
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("COSH_DATA_DIR", root.path().join("data"))
                .env("COSH_CONFIG_DIR", root.path().join("config"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{enabled}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        }
        return;
    }

    let mut app = App::new("/tmp".into());
    let enabled = app.telemetry.enabled();
    let mut terminal = Terminal::new(TestBackend::new(140, 50)).unwrap();
    // Configured means all configured servers, including disabled entries;
    // the telemetry records only the count, never a name or URL.
    app.setup.mcp.servers = serde_json::from_value(serde_json::json!([
        {"name":"private-server", "enabled":false,
         "transport":{"type":"http", "url":"https://example.invalid"}}
    ]))
    .unwrap();

    #[cfg(feature = "home")]
    {
        use crate::routes::home::BannerContent;
        app.home_view.banner.content = Some(BannerContent::ReleaseUpdate {
            version: "9.0.0".into(),
            tag: "v9.0.0".into(),
        });
        let mut small = Terminal::new(TestBackend::new(20, 10)).unwrap();
        small.draw(|f| app.render(f, 0.0)).unwrap();
        assert!(!app.home_view.banner.is_visible());
        terminal.draw(|f| app.render(f, 0.0)).unwrap();
        assert!(app.home_view.banner.is_visible());
    }
    app.show_settings = true;
    terminal.draw(|f| app.render(f, 0.0)).unwrap();
    terminal.draw(|f| app.render(f, 0.0)).unwrap();
    app.show_settings = false;
    app.show_internal_tools = true;
    terminal.draw(|f| app.render(f, 0.0)).unwrap();
    app.show_internal_tools = false;
    app.show_add_provider = true;
    terminal.draw(|f| app.render(f, 0.0)).unwrap();
    app.show_add_provider = false;

    let id = "telemetry-synthetic-session".to_string();
    app.state.add_empty_session(id.clone(), "test".into(), 0);
    app.state.current_session_id = Some(id);
    terminal.draw(|f| app.render(f, 0.0)).unwrap();
    for event in [
        HarnessEvent::UserMessageInjected {
            text: "private prompt".into(),
        },
        HarnessEvent::BeginAssistant,
        HarnessEvent::Token {
            text: "private response".into(),
        },
        HarnessEvent::Token {
            text: " another chunk".into(),
        },
        HarnessEvent::ToolCall {
            tool: "bash_run".into(),
            input: serde_json::json!({}),
        },
        HarnessEvent::ToolError {
            error: "private tool failure".into(),
        },
        HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Started,
        },
        HarnessEvent::ContextItemRecorded {
            item_id: 1,
            user: false,
            call_item_id: None,
            tool_name: None,
            compaction: true,
        },
        HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Finished,
        },
        HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Finished,
        },
        HarnessEvent::CompactOnDemand {
            outcome: cosh::harness::ManualCompactionOutcome::Compacted,
        },
        HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Failed,
        },
        HarnessEvent::Error {
            message: "private fatal error".into(),
            context: None,
            checkup_verdict: None,
        },
    ] {
        app.event_tx.send(event).unwrap();
        app.poll_events();
    }
    let summary = std::mem::take(&mut app.session_telemetry).finish();
    assert_eq!(summary.messages(), if enabled { 4 } else { 0 });
    assert_eq!(summary.compactions(), u32::from(enabled));
    assert_eq!(summary.mcp_servers(), u32::from(enabled));
    assert_eq!(
        summary.update_banner_shown(),
        enabled && cfg!(feature = "home")
    );
    if enabled {
        assert_eq!(summary.tools().get("bash"), Some(&1));
        for feature in ["settings", "tools", "add_provider", "session", "prompt"] {
            assert_eq!(summary.features().get(feature), Some(&1), "{feature}");
        }
        assert_eq!(summary.errors().len(), 2);
        assert!(
            summary
                .errors()
                .iter()
                .any(|e| e.category() == ErrorCategory::Unknown)
        );
        assert!(
            summary
                .errors()
                .iter()
                .any(|e| e.category() == ErrorCategory::ToolFailed)
        );
    } else {
        assert!(summary.tools().is_empty());
        assert!(summary.features().is_empty());
        assert!(summary.errors().is_empty());
    }
    let wire = serde_json::to_string(&summary).unwrap();
    assert!(
        !wire.contains("private"),
        "raw input must never enter telemetry"
    );
}
