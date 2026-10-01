use super::*;

// --------------------------------------------------------------- list criteria and instructions
#[test]
fn list_instructions_change_the_query_and_the_winner() {
    let list_embed = TableEmbed::from_pairs(&[
        ("Classify\npay me", vec![0.0, 1.0]),
        ("alpha", vec![1.0, 0.0]),
        ("beta", vec![0.0, 1.0]),
        ("gamma", vec![0.0, 0.2]),
    ]);
    let labels = shortlist_choice(
        &json!("pay me"),
        &json!(["alpha", "beta", "gamma"]),
        &list_embed,
        2,
        Some("Classify"),
    )
    .expect("shortlist");
    assert_eq!(
        labels,
        [json!("beta"), json!("gamma")],
        "list/instructions change the query and the winner"
    );
    assert_eq!(
        list_embed.calls()[0][0],
        "Classify\npay me",
        "list/query text includes instructions"
    );
    assert_eq!(
        list_embed.calls()[0][1..],
        ["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        "list/option texts are the labels"
    );
}

#[test]
fn dict_state_is_serialized() {
    let state = json!({"text": "hi"});
    let dict_embed = TableEmbed::from_pairs(&[
        ("Classify\n{\"text\": \"hi\"}", vec![1.0, 0.0]),
        ("alpha", vec![1.0, 0.0]),
        ("beta", vec![0.0, 1.0]),
    ]);
    let labels = shortlist_choice(
        &state,
        &json!(["alpha", "beta"]),
        &dict_embed,
        1,
        Some("Classify"),
    )
    .expect("shortlist");
    assert_eq!(labels, [json!("alpha")], "query/dict state is serialized");
    assert_eq!(
        dict_embed.calls()[0][0],
        format!("Classify\n{}", serialize_state(&state)),
        "query/json matches serialize_state"
    );
}

#[test]
fn render_zero_and_false_stay_in_the_option_text() {
    // 0 and False are real criterion values, so they are part of the
    // embedded text
    let rich = json!({"zero": 0, "no": false, "bare": null, "named": {"desc": "payments"}});
    let rich_rendered =
        render_options(&json!({"t": "choice", "ins": "", "crit": rich})).expect("render");
    let mut texts = vec!["pay me".to_string()];
    texts.extend(rich_rendered.iter().cloned());
    let rich_embed = TableEmbed::new(
        std::iter::once(("pay me".to_string(), vec![1.0, 0.0]))
            .chain(rich_rendered.iter().map(|t| (t.clone(), vec![1.0, 0.0])))
            .collect(),
    );
    shortlist(&json!("pay me"), &rich, &rich_embed, 1).expect("shortlist");
    assert_eq!(
        rich_embed.calls()[0][1..],
        rich_rendered[..],
        "render/0 and False stay in the option text"
    );
}

// --------------------------------------------------------------- k >= n pass-through
#[test]
fn pass_through_returns_every_label_in_order() {
    let boom = BoomEmbed;
    let criteria = criteria_value();
    let expected: Vec<Value> = criteria
        .as_object()
        .expect("map")
        .keys()
        .map(|k| json!(k))
        .collect();
    assert_eq!(
        shortlist(&json!("pay me"), &criteria, &boom, 4).expect("k == n"),
        expected,
        "pass/k == n returns every label in order"
    );
    assert_eq!(
        shortlist(&json!("pay me"), &criteria, &boom, 20).expect("k > n"),
        expected,
        "pass/k > n returns every label in order"
    );
}
