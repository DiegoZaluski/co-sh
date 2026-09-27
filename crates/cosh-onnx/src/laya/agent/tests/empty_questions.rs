use super::*;

// ------------------------------------------------- test_empty_questions.py (ONNX half)
#[test]
fn empty_questions_return_the_empty_response_for_supported_states() {
    let agent = bare_onnx(
        json!({}),
        vec![vec![2.0, 1.0, 0.0], vec![2.0, 0.0, 0.0]],
        vec![vec![0.7, 0.3], vec![0.4, 0.6]],
    );
    let empty = json!({
        "model": "laya-rl-agent-onnx",
        "answers": {},
        "usage": {"input_tokens": 0, "output_tokens": 0}
    });
    for state in [
        json!("hello"),
        json!({"text": "hello"}),
        json!([{"role": "user", "content": "hello"}]),
        json!(""),
        json!({}),
        json!([]),
    ] {
        let got = agent.infer(&state, &Map::new(), None, None, None).unwrap();
        assert_eq!(got, empty, "state: {}", state);
    }
}

#[test]
fn empty_responses_do_not_share_mutable_containers() {
    let agent = bare_onnx(json!({}), vec![], vec![]);
    let empty = json!({
        "model": "laya-rl-agent-onnx",
        "answers": {},
        "usage": {"input_tokens": 0, "output_tokens": 0}
    });
    let mut first = agent.infer(&json!("hello"), &Map::new(), None, None, None).unwrap();
    first["answers"]["changed"] = json!(true);
    first["usage"]["input_tokens"] = json!(7);
    let second = agent.infer(&json!("hello"), &Map::new(), None, None, None).unwrap();
    assert_eq!(second, empty);
}

