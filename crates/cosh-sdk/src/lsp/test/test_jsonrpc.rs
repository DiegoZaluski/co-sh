//! Unit tests for JSON-RPC message classification and encoding.

use std::path::Path;

use crate::lsp::jsonrpc::{
    IncomingMessage, RequestId, RpcError, classify_message, encode_request, encode_response_err,
    encode_response_ok, error_codes,
};
use serde_json::{Value, json};

#[test]
fn classify_request_notification_and_response() {
    let msg = classify_message(
        r#"{"jsonrpc":"2.0","id":2,"method":"workspace/configuration","params":{"items":[]}}"#,
    )
    .unwrap();
    assert!(matches!(
        msg,
        IncomingMessage::Request { id: RequestId::Number(2), ref method, .. }
            if method.as_ref() == "workspace/configuration"
    ));

    let msg = classify_message(
        r#"{"jsonrpc":"2.0","method":"publishDiagnostics","params":{"uri":"file:///a"}}"#,
    )
    .unwrap();
    assert!(
        matches!(msg, IncomingMessage::Notification { ref method, .. } if method.as_ref() == "publishDiagnostics")
    );

    let msg = classify_message(r#"{"jsonrpc":"2.0","id":7,"result":{"x":1}}"#).unwrap();
    assert!(matches!(
        msg,
        IncomingMessage::Response {
            id: RequestId::Number(7),
            outcome: Ok(_)
        }
    ));

    // `result: null` (e.g. the shutdown reply) is success with a null value.
    let msg = classify_message(r#"{"jsonrpc":"2.0","id":7,"result":null}"#).unwrap();
    assert!(matches!(
        msg,
        IncomingMessage::Response {
            outcome: Ok(Value::Null),
            ..
        }
    ));
}

/// Mirrors zed's `test_deserialize_string_digit_id`: string ids are never
/// coerced to numbers.
#[test]
fn classify_string_digit_id_is_not_coerced() {
    let msg = classify_message(r#"{"jsonrpc":"2.0","id":"2","method":"m"}"#).unwrap();
    assert!(matches!(
        msg,
        IncomingMessage::Request { id: RequestId::String(ref s), .. } if s == "2"
    ));
}

#[test]
fn classify_error_response() {
    let msg = classify_message(
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope","data":null}}"#,
    )
    .unwrap();
    let IncomingMessage::Response {
        outcome: Err(err), ..
    } = msg
    else {
        panic!("expected error response")
    };
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    assert_eq!(err.message, "nope");
}

/// Malformed frames must be classifiable failures — never a panic — so the
/// reader loop can log-and-skip them.
#[test]
fn classify_malformed_messages_are_rejected_not_fatal() {
    assert!(classify_message("not json at all").is_err());
    // No method and no id: unclassifiable.
    assert!(classify_message(r#"{"jsonrpc":"2.0"}"#).is_err());
}

#[test]
fn encode_request_omits_absent_params() {
    let body = encode_request(&RequestId::Number(1), "shutdown", None);
    assert_eq!(body, r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#);

    let params = json!({"processId": 42});
    let body = encode_request(&RequestId::Number(2), "initialize", Some(&params));
    assert_eq!(
        body,
        r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"processId":42}}"#
    );
}

/// Mirrors zed ticket #10595: a response must never serialize both `result`
/// and `error` keys.
#[test]
fn encode_response_never_has_both_result_and_error() {
    let ok_body = encode_response_ok(&RequestId::Number(0), &Value::Null);
    assert_eq!(ok_body, r#"{"jsonrpc":"2.0","id":0,"result":null}"#);

    let err = encode_response_err(
        &RequestId::Number(0),
        &RpcError::new(error_codes::METHOD_NOT_FOUND, "Unrecognized method"),
    );
    assert!(!err.contains("\"result\""), "{err}");
    assert!(err.contains("\"error\""));
}

/// uri_from_path percent-encoding (spaces, accents, windows drive).
#[test]
fn uri_from_path_encodes_special_characters() {
    use crate::lsp::client::uri_from_path;

    let uri = uri_from_path(Path::new("/tmp/my project")).unwrap();
    assert_eq!(uri.as_str(), "file:///tmp/my%20project");

    let uri = uri_from_path(Path::new("/tmp/caf\u{00e9}")).unwrap();
    assert_eq!(uri.as_str(), "file:///tmp/caf%C3%A9");

    #[cfg(windows)]
    {
        use std::path::PathBuf;
        let uri = uri_from_path(PathBuf::from(r"C:\\src\\proj")).unwrap();
        assert_eq!(uri.as_str(), "file:///C:/src/proj");
    }
}
