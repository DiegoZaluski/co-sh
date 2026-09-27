use super::*;

// --------------------------------------------------------------- deterministic top-k
#[test]
fn topk_keeps_the_cosine_tie_in_input_order() {
    // `_embed_for("pay me", OPTION_TEXTS)`: the query vector rides on top of
    // the option vectors.
    let mut query_and_options = vec![("pay me", vec![1.0, 0.0])];
    query_and_options.extend(option_texts_table());
    let embed = TableEmbed::from_pairs(&query_and_options);
    let criteria = criteria_value();
    let labels = shortlist(&json!("pay me"), &criteria, &embed, 2).expect("shortlist");
    assert_eq!(
        labels,
        [json!("alpha"), json!("delta")],
        "topk/k=2 keeps the cosine tie in input order"
    );
    assert_eq!(embed.calls().len(), 1, "topk/one embed call");
    assert_eq!(embed.calls()[0][0], "pay me", "topk/query is the first text");
    assert_eq!(
        embed.calls()[0][1..],
        render_options(&json!({"t": "choice", "ins": "", "crit": criteria})).expect("render"),
        "topk/option texts match render_options"
    );
    assert_eq!(
        shortlist(&json!("pay me"), &criteria, &embed, 1).expect("k=1"),
        [json!("alpha")],
        "topk/k=1 is the earliest max"
    );
    assert_eq!(
        shortlist(&json!("pay me"), &criteria, &embed, 3).expect("k=3"),
        [json!("alpha"), json!("delta"), json!("gamma")],
        "topk/k=3 appends the next cosine"
    );
}

#[test]
fn topk_zero_query_keeps_original_order() {
    // all-zero query: every cosine is 0, so the earliest labels win
    let zero_q = TableEmbed::from_pairs(&[
        ("pay me", vec![0.0, 0.0]),
        ("alpha", vec![1.0, 0.0]),
        ("beta", vec![0.0, 1.0]),
        ("gamma: mid", vec![0.6, 0.8]),
        ("delta: same", vec![3.0, 4.0]),
    ]);
    assert_eq!(
        shortlist(&json!("pay me"), &criteria_value(), &zero_q, 2).expect("shortlist"),
        [json!("alpha"), json!("beta")],
        "topk/zero query keeps original order"
    );
}

#[test]
fn topk_nan_vector_sorts_behind_a_finite_match() {
    // non-finite option vector is treated as 0 and loses to a real match
    let nan_embed = TableEmbed::from_pairs(&[
        ("pay me", vec![1.0, 0.0]),
        ("alpha", vec![f64::NAN, f64::NAN]),
        ("beta", vec![1.0, 0.0]),
    ]);
    assert_eq!(
        shortlist(
            &json!("pay me"),
            &json!({"alpha": null, "beta": null}),
            &nan_embed,
            1
        )
        .expect("shortlist"),
        [json!("beta")],
        "topk/nan vector sorts behind a finite match"
    );
}

