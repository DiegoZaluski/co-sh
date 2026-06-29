use super::TerminalInput;

#[test]
fn terminal_processes_simple_output() {
    let result = super::terminal(&TerminalInput {
        output: "hello\nworld".into(),
        rows: 5,
        cols: 20,
        plain: true,
    })
    .unwrap();
    // With newline mode enabled, `\n` should reset to column 0
    let lines: Vec<&str> = result.lines().collect();
    assert_eq!(lines[0].trim(), "hello", "first line should start with 'hello'");
    assert_eq!(lines[1].trim(), "world", "second line should start with 'world'");
}

#[test]
fn terminal_processes_cursor_movement() {
    let result = super::terminal(&TerminalInput {
        output: "abc\r\n\x1b[2;1Hdef".into(),
        rows: 3,
        cols: 10,
        plain: true,
    })
    .unwrap();
    assert!(result.contains("abc"));
    assert!(result.contains("def"));
}

#[test]
fn vision_terminal_method_works() {
    let vision = super::Vision::new();
    let result = vision
        .terminal(&TerminalInput {
            output: "xyz".into(),
            rows: 3,
            cols: 10,
            plain: false,
        })
        .unwrap();
    assert!(result.contains("\"content\""));
    assert!(result.contains("xyz"));
}

#[test]
fn terminal_detailed_returns_metadata() {
    let result = super::terminal(&TerminalInput {
        output: "abc".into(),
        rows: 3,
        cols: 10,
        plain: false,
    })
    .unwrap();
    assert!(result.contains("\"rows\""));
    assert!(result.contains("\"cols\""));
    assert!(result.contains("\"cursor_x\""));
    assert!(result.contains("\"cells\""));
}

#[test]
fn terminal_includes_width() {
    let result = super::terminal(&TerminalInput {
        output: "hi".into(),
        rows: 3,
        cols: 10,
        plain: false,
    })
    .unwrap();
    assert!(result.contains("\"width\""));
}

#[test]
fn terminal_includes_hyperlink_field_when_present() {
    // OSC 8 hyperlink: \x1b]8;uri=https://example.com;Hello\x1b]8;;
    let result = super::terminal(&TerminalInput {
        output: "\x1b]8;;https://example.com\x1b\\Hello\x1b]8;;\x1b\\".into(),
        rows: 3,
        cols: 20,
        plain: false,
    })
    .unwrap();
    assert!(result.contains("\"hyperlink\""));
    assert!(result.contains("https://example.com"));
}

#[test]
fn terminal_skips_hyperlink_when_absent() {
    let result = super::terminal(&TerminalInput {
        output: "test".into(),
        rows: 3,
        cols: 10,
        plain: false,
    })
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    // Ensure no cell has a hyperlink field
    for cell in parsed["cells"].as_array().unwrap() {
        assert!(cell.get("hyperlink").is_none());
    }
}

#[test]
fn terminal_cursor_shape_conditional() {
    let result = super::terminal(&TerminalInput {
        output: "test".into(),
        rows: 3,
        cols: 10,
        plain: false,
    })
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    // cursor_shape should not be present when it's Default
    assert!(parsed.get("cursor_shape").is_none());
}
