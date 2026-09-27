use super::*;

// ------------------------------------------------- test_temperature_loading.py (parsing half)
/// `load_config` without the checkpoint: parse the temperatures out of a
/// config object exactly as the loader does, and collect the warning.
fn load_temps(cfg: Value) -> (OnnxAgent, Option<String>) {
    let mut agent = bare_agent(
        cfg.as_object().cloned().unwrap_or_default(),
        Box::new(FakeTok),
        Box::new(StubSession { logits: vec![], act_logits: vec![] }),
        [1.0, 1.0, 1.0],
        HashMap::new(),
        HashMap::new(),
    );
    agent.apply_config_temperatures().unwrap();
    let warning = agent.temperature_warning();
    (agent, warning)
}

#[test]
fn invalid_type_entries_use_neutral_fallback_and_warn() {
    for value in [json!(null), json!("invalid"), json!(""), json!([]), json!({})] {
        let (agent, warning) = load_temps(json!({"temperature": [value, 2.0, 3.0]}));
        assert_eq!(agent.temperature, [1.0, 2.0, 3.0], "value: {}", value);
        let msg = warning.expect("a warning was expected");
        assert!(msg.contains("temperature[0]"), "{}", msg);
        assert!(msg.contains(&py_repr_value(&value)), "{}", msg);
        assert!(msg.contains("uncalibrated"), "{}", msg);
    }
}

#[test]
fn invalid_bucket_entries_use_neutral_fallback_and_keep_precedence() {
    for value in [json!(null), json!("invalid"), json!(""), json!([]), json!({})] {
        let (agent, warning) = load_temps(json!({
            "temperature": [2.0, 3.0, 4.0],
            "temperature_by_options": {"choice:2": value}
        }));
        assert_eq!(
            agent.temperature_by_options.get("choice:2").copied(),
            Some(1.0),
            "value: {}",
            value
        );
        let msg = warning.expect("a warning was expected");
        assert!(msg.contains("choice:2"), "{}", msg);
        assert!(msg.contains(&py_repr_value(&value)), "{}", msg);
    }
}

#[test]
fn nonfinite_values_keep_neutral_fallback_and_warn() {
    // On the JSON surface the non-finite floats arrive as the strings
    // float("NaN")/float("inf") accept; the float-object forms cannot be
    // expressed in serde_json.
    for value in ["NaN", "Infinity", "-Infinity"] {
        let (agent, warning) = load_temps(json!({
            "temperature": [value, 2.0, 3.0],
            "temperature_by_options": {"noul:2": value}
        }));
        assert_eq!(agent.temperature, [1.0, 2.0, 3.0], "value: {}", value);
        assert_eq!(
            agent.temperature_by_options.get("noul:2").copied(),
            Some(1.0),
            "value: {}",
            value
        );
        let msg = warning.expect("a warning was expected");
        assert!(msg.contains("temperature[0]"), "{}", msg);
        assert!(msg.contains("noul:2"), "{}", msg);
    }
}

#[test]
fn out_of_range_values_keep_existing_clamps() {
    let (agent, warning) = load_temps(json!({
        "temperature": [0.1, 10, -1],
        "temperature_by_options": {"choice:2": 9, "score:3-5": 0}
    }));
    assert_eq!(agent.temperature, [0.5, 5.0, 0.5]);
    assert_eq!(
        agent.temperature_by_options.get("choice:2").copied(),
        Some(5.0)
    );
    assert_eq!(
        agent.temperature_by_options.get("score:3-5").copied(),
        Some(0.5)
    );
    let msg = warning.expect("a warning was expected");
    for entry in ["temperature[0]", "temperature[1]", "temperature[2]", "choice:2", "score:3-5"] {
        assert!(msg.contains(entry), "{} -> {}", entry, msg);
    }
}

#[test]
fn valid_numbers_and_numeric_strings_do_not_warn() {
    let (agent, warning) = load_temps(json!({
        "temperature": [0.5, "2", 5],
        "temperature_by_options": {"choice:2": "1.5", "score:3-5": 2.5, "noul:2": "5.0"}
    }));
    assert_eq!(agent.temperature, [0.5, 2.0, 5.0]);
    assert_eq!(
        agent.temperature_by_options.get("choice:2").copied(),
        Some(1.5)
    );
    assert_eq!(
        agent.temperature_by_options.get("score:3-5").copied(),
        Some(2.5)
    );
    assert_eq!(
        agent.temperature_by_options.get("noul:2").copied(),
        Some(5.0)
    );
    assert!(warning.is_none(), "{}", warning.unwrap());
}

#[test]
fn missing_temperature_fields_keep_defaults() {
    let (agent, warning) = load_temps(json!({}));
    assert_eq!(agent.temperature, [1.0, 1.0, 1.0]);
    assert!(agent.temperature_by_options.is_empty());
    assert!(warning.is_none());
}
