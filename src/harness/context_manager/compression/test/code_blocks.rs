use super::super::init;

#[test]
fn code_with_backticks_preserves_content() {
    let text = "Here is some code:\n```rust\nfn hello() {\n    println!(\"world\");\n}\n```\nThis is the conclusion.";
    let result = init(text, false, 0.8, 0.7, 0.4);
    assert!(!result.is_empty(), "code blocks should be handled");
    assert!(
        result.contains("hello") || result.contains("conclusion"),
        "output should retain content from code block or surrounding text"
    );
}

#[test]
fn inline_code_preserves_text() {
    let text = "Use `let x = 42;` to bind a variable. Then `println!` to print.";
    let result = init(text, false, 0.8, 0.7, 0.5);
    assert!(!result.is_empty(), "inline code should not crash");
    assert!(
        result.contains("let") || result.contains("variable") || result.contains("bind"),
        "output should retain some original content"
    );
}

#[test]
fn json_in_text_preserves_content() {
    let text = r#"The response was {"status": "ok", "data": [1, 2, 3]}. This is valid."#;
    let result = init(text, false, 0.8, 0.7, 0.5);
    assert!(!result.is_empty(), "JSON should not crash");
    assert!(
        result.contains("response") || result.contains("status") || result.contains("valid"),
        "output should retain some words from input"
    );
}

#[test]
fn mixed_code_and_text_preserves_content() {
    let text = "First we define a function.\n```\ndef foo():\n    pass\n```\nThen we call it. That is all.";
    let result = init(text, false, 0.8, 0.7, 0.4);
    assert!(!result.is_empty(), "mixed code/text should compress");
    assert!(
        result.contains("define") || result.contains("function") || result.contains("call"),
        "output should retain some original content"
    );
}
