use super::*;

// --------------------------------------------------------------- end-to-end helper: shortlist then the question normalizer only
#[test]
fn pipe_reduced_choice_scores_inside_the_shortlist() {
    // Confirms a reduced choice still has one criterion per kept label, which
    // is what system_one would score. No weights, no forward.
    let (full, _sentinel) = full_criteria();
    let pipe_q = json!({
        "intent": {"type": "choice", "instructions": "Which desk?", "criteria": full},
        "urgency": score_question(),
    });
    let agent = Recorder::new();
    let piped = predict_shortlist(
        &agent,
        &json!("I was charged twice"),
        pipe_q.as_object().expect("map"),
        &TableEmbed::from_pairs(&full_vectors()),
        2,
    )
    .expect("predict_shortlist");
    let scored_questions = {
        let calls = agent.calls.lock().unwrap_or_else(|e| e.into_inner());
        calls[0].1.clone()
    };
    let scored = to_internal(&scored_questions["intent"]).expect("internal");
    assert_eq!(
        scored["crit"].as_object().expect("map").len(),
        2,
        "pipe/marker count equals k"
    );
    assert!(
        piped["shortlist"]["intent"]["labels"]
            .as_array()
            .expect("labels")
            .contains(&piped["answers"]["intent"]["choice"]),
        "pipe/answer choice is inside the shortlist"
    );
}

// --------------------------------------------------------------- default k
#[test]
fn default_shortlist_k_is_twenty() {
    assert_eq!(DEFAULT_SHORTLIST_K, 20, "export/default k");
}

// --------------------------------------------------------------- cached_embed_fn
fn cache_vectors() -> HashMap<String, Vec<f64>> {
    let mut v = vec_table(&option_texts_table());
    v.insert("refund please".to_string(), vec![0.0, 1.0]);
    v
}

const CACHE_TEXTS: [&str; 5] = ["pay me", "alpha", "beta", "gamma: mid", "delta: same"];

#[test]
fn cache_cold_call_embeds_every_text_once() {
    let table = Arc::new(TableEmbed::new(cache_vectors()));
    let wrapped = cached_embed_fn(SharedEmbed(Arc::clone(&table)), 4096).expect("cached_embed_fn");
    let texts: Vec<String> = CACHE_TEXTS.iter().map(|s| (*s).to_string()).collect();
    let first = wrapped.embed(&texts).expect("cold call");
    assert_eq!(
        table.calls(),
        [CACHE_TEXTS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()],
        "cache/cold call embeds every text once"
    );
    assert_eq!(first.len(), 5, "cache/cold output shape");
    assert!(
        first.iter().all(|row| row.len() == 2),
        "cache/cold output shape (rows are 2-wide)"
    );
    let table2 = TableEmbed::new(cache_vectors());
    for t in CACHE_TEXTS {
        let _ = table2.vectors[t];
    }
    let unwrapped = TableEmbed::new(cache_vectors());
    let direct = unwrapped.embed(&texts).expect("unwrapped");
    assert_eq!(first, direct, "cache/cold values match unwrapped");
    let second = wrapped.embed(&texts).expect("warm call");
    assert_eq!(
        table.calls().len(),
        1,
        "cache/warm call makes no embed call"
    );
    assert_eq!(first, second, "cache/warm returns identical values");
}

#[test]
fn cache_repeat_embeds_only_the_new_text() {
    let table = Arc::new(TableEmbed::new(cache_vectors()));
    let wrapped = cached_embed_fn(SharedEmbed(Arc::clone(&table)), 4096).expect("cached_embed_fn");
    let all: Vec<String> = CACHE_TEXTS.iter().map(|s| (*s).to_string()).collect();
    let _ = wrapped.embed(&all).expect("cold");
    let third = wrapped
        .embed(&[
            "refund please".to_string(),
            "alpha".to_string(),
            "beta".to_string(),
        ])
        .expect("third");
    let calls = table.calls();
    assert_eq!(
        calls.last().expect("last call"),
        &["refund please".to_string()],
        "cache/repeat embeds only the new text"
    );
    assert_eq!(calls.len(), 2, "cache/underlying calls total");
    assert_eq!(
        third,
        [
            vec![0.0, 1.0], // refund please
            vec![1.0, 0.0], // alpha
            vec![0.0, 1.0], // beta
        ],
        "cache/partial rows keep request order"
    );
}

#[test]
fn cache_duplicate_text_embedded_once_per_call() {
    let table = Arc::new(TableEmbed::new(cache_vectors()));
    let dup_cached =
        cached_embed_fn(SharedEmbed(Arc::clone(&table)), 4096).expect("cached_embed_fn");
    let dup = dup_cached
        .embed(&["alpha".to_string(), "beta".to_string(), "alpha".to_string()])
        .expect("dup");
    assert_eq!(
        table.calls(),
        [vec!["alpha".to_string(), "beta".to_string()]],
        "cache/duplicate text embedded once per call"
    );
    assert_eq!(dup.len(), 3, "cache/duplicate output keeps request length");
    assert_eq!(dup[0], dup[2], "cache/duplicate rows repeat the vector");
}

#[test]
fn cache_lru_eviction() {
    let table = Arc::new(TableEmbed::new(cache_vectors()));
    let lru_cached = cached_embed_fn(SharedEmbed(Arc::clone(&table)), 2).expect("cached_embed_fn");
    lru_cached
        .embed(&["alpha".to_string(), "beta".to_string()])
        .expect("1"); // cache: alpha, beta
    lru_cached.embed(&["alpha".to_string()]).expect("2"); // hit; alpha now newest, beta is LRU
    assert_eq!(table.calls().len(), 1, "cache/lru touch needs no embed");
    lru_cached.embed(&["gamma: mid".to_string()]).expect("3"); // inserts gamma, evicts beta
    assert_eq!(
        table.calls().last().expect("last"),
        &["gamma: mid".to_string()],
        "cache/lru insert embeds the new text"
    );
    lru_cached
        .embed(&["alpha".to_string(), "gamma: mid".to_string()])
        .expect("4"); // both hits
    assert_eq!(
        table.calls().len(),
        2,
        "cache/lru survivors are both cached"
    );
    lru_cached.embed(&["beta".to_string()]).expect("5"); // beta was evicted
    assert_eq!(
        table.calls().last().expect("last"),
        &["beta".to_string()],
        "cache/lru evicted entry is re-embedded"
    );
    assert_eq!(
        lru_cached.cache_info()["size"],
        json!(2),
        "cache/lru size stays at the bound"
    );
    lru_cached
        .embed(&["gamma: mid".to_string(), "beta".to_string()])
        .expect("6"); // both hits
    assert_eq!(
        table.calls().len(),
        3,
        "cache/lru most recent pair survives"
    );
    lru_cached.embed(&["alpha".to_string()]).expect("7"); // alpha was the oldest of the three
    assert_eq!(
        table.calls().last().expect("last"),
        &["alpha".to_string()],
        "cache/lru oldest of three was evicted"
    );
}

#[test]
fn cache_info_and_clear() {
    let table = Arc::new(TableEmbed::new(cache_vectors()));
    let info_cached = cached_embed_fn(SharedEmbed(Arc::clone(&table)), 8).expect("cached_embed_fn");
    info_cached
        .embed(&["alpha".to_string(), "beta".to_string()])
        .expect("1"); // 2 misses
    info_cached
        .embed(&["alpha".to_string(), "gamma: mid".to_string()])
        .expect("2"); // 1 hit + 1 miss
    let info = info_cached.cache_info();
    assert_eq!(info["size"], json!(3), "cache/info size");
    assert_eq!(info["maxsize"], json!(8), "cache/info maxsize");
    assert_eq!(info["hits"], json!(1), "cache/info hits");
    assert_eq!(info["misses"], json!(3), "cache/info misses");
    info_cached.cache_clear();
    let cleared = info_cached.cache_info();
    assert_eq!(
        cleared,
        {
            let mut m = Map::new();
            m.insert("size".to_string(), json!(0));
            m.insert("maxsize".to_string(), json!(8));
            m.insert("hits".to_string(), json!(0));
            m.insert("misses".to_string(), json!(0));
            m
        },
        "cache/clear resets info"
    );
    info_cached.embed(&["alpha".to_string()]).expect("3");
    assert_eq!(
        table.calls().last().expect("last"),
        &["alpha".to_string()],
        "cache/clear forces re-embed"
    );
}

#[test]
fn cache_maxsize_zero_rejected() {
    let table = Arc::new(TableEmbed::new(HashMap::new()));
    let err = cached_embed_fn(SharedEmbed(Arc::clone(&table)), 0)
        .err()
        .expect("maxsize zero");
    assert!(
        matches!(err, Error::Value(ref m) if m.contains("maxsize must be a positive integer")),
        "cache/maxsize zero rejected: {err:?}"
    );
}

struct FlakyEmbed {
    calls: std::sync::atomic::AtomicUsize,
}

impl EmbedFn for FlakyEmbed {
    /// Fails on the first call, succeeds after.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call == 1 {
            return Err(Error::Runtime("boom".to_string()));
        }
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

#[test]
fn cache_embed_error_propagates_and_caches_nothing() {
    let flaky = FlakyEmbed {
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let flaky_cached = cached_embed_fn(flaky, 4096).expect("cached_embed_fn");
    let err = flaky_cached
        .embed(&["alpha".to_string()])
        .expect_err("first call fails");
    assert!(
        matches!(err, Error::Runtime(_)),
        "cache/embed error propagates"
    );
    assert_eq!(
        flaky_cached.cache_info()["size"],
        json!(0),
        "cache/failed call caches nothing"
    );
    let retry = flaky_cached.embed(&["alpha".to_string()]).expect("retry");
    assert_eq!(
        flaky_cached.cache_info()["size"],
        json!(1),
        "cache/retried row is cached"
    );
    assert_eq!(
        retry,
        [vec![1.0, 0.0]],
        "cache/retried row is the embedded one"
    );
}

struct BadShapeEmbed;

impl EmbedFn for BadShapeEmbed {
    /// One row no matter how many texts arrive.
    fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f64>>> {
        Ok(vec![vec![1.0, 0.0]])
    }
}

#[test]
fn cache_bad_shape_raises_and_caches_nothing() {
    let bad_cached = cached_embed_fn(BadShapeEmbed, 4096).expect("cached_embed_fn");
    let err = bad_cached
        .embed(&["alpha".to_string(), "beta".to_string()])
        .expect_err("bad shape");
    assert!(matches!(err, Error::Value(_)), "cache/bad shape raises");
    assert_eq!(
        bad_cached.cache_info()["size"],
        json!(0),
        "cache/bad shape caches nothing"
    );
}

#[test]
fn cache_empty_string_key_and_empty_input() {
    let none_table = Arc::new(TableEmbed::from_pairs(&[("", vec![1.0, 0.0])]));
    let none_cached =
        cached_embed_fn(SharedEmbed(Arc::clone(&none_table)), 4096).expect("cached_embed_fn");
    none_cached.embed(&["".to_string()]).expect("empty string");
    assert_eq!(
        none_table.calls(),
        [vec!["".to_string()]],
        "cache/empty string embeds once"
    );
    none_cached.embed(&["".to_string()]).expect("hit");
    assert_eq!(
        none_table.calls().len(),
        1,
        "cache/empty string hits the same entry"
    );
    assert_eq!(
        none_cached.embed(&[]).expect("empty input").len(),
        0,
        "cache/empty input embeds nothing"
    );
}

struct NanEmbed;

impl EmbedFn for NanEmbed {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        Ok(texts
            .iter()
            .map(|t| {
                if t == "alpha" {
                    vec![f64::NAN, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
    }
}

#[test]
fn cache_nan_row_stored_cleaned() {
    let nan_cached = cached_embed_fn(NanEmbed, 4096).expect("cached_embed_fn");
    let out = nan_cached
        .embed(&["alpha".to_string(), "beta".to_string()])
        .expect("first");
    assert!(
        out.iter().all(|row| row.iter().all(|v| v.is_finite())),
        "cache/nan row stored cleaned"
    );
    let out2 = nan_cached
        .embed(&["alpha".to_string(), "beta".to_string()])
        .expect("second");
    assert_eq!(out, out2, "cache/cleaned row served from cache identical");
}

// --------------------------------------------------------------- cached embedder inside the shortlist
#[test]
fn cache_end_to_end_with_shortlist() {
    let e2e_table = Arc::new(TableEmbed::new(cache_vectors()));
    let e2e_cached =
        cached_embed_fn(SharedEmbed(Arc::clone(&e2e_table)), 4096).expect("cached_embed_fn");
    let run1 = shortlist(&json!("pay me"), &criteria_value(), &e2e_cached, 2).expect("run1");
    let run2 = shortlist(&json!("refund please"), &criteria_value(), &e2e_cached, 2).expect("run2");
    assert_eq!(
        e2e_table.calls()[0],
        CACHE_TEXTS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "cache/e2e first call embeds query and options"
    );
    assert_eq!(
        e2e_table.calls()[1],
        ["refund please".to_string()],
        "cache/e2e repeat embeds only the new query"
    );
    assert_eq!(e2e_table.calls().len(), 2, "cache/e2e two calls total");
    assert_eq!(
        run1,
        [json!("alpha"), json!("delta")],
        "cache/e2e labels match the uncached path"
    );
    assert_eq!(
        run2,
        [json!("beta"), json!("gamma")],
        "cache/e2e second query labels"
    );
}

#[test]
fn cache_inside_predict_shortlist() {
    let mut pipe_vectors = cache_vectors();
    pipe_vectors.insert("category\npay me".to_string(), vec![1.0, 0.0]);
    pipe_vectors.insert("category\nrefund please".to_string(), vec![0.0, 1.0]);
    let pipe_table = Arc::new(TableEmbed::new(pipe_vectors));
    let pipe_cached =
        cached_embed_fn(SharedEmbed(Arc::clone(&pipe_table)), 4096).expect("cached_embed_fn");
    let pipe_q = json!({
        "intent": {"type": "choice", "instructions": "category", "criteria": criteria_value()}
    });
    let agent = Recorder::new();
    predict_shortlist(
        &agent,
        &json!("pay me"),
        pipe_q.as_object().expect("map"),
        &pipe_cached,
        2,
    )
    .expect("first");
    let pipe_b = predict_shortlist(
        &agent,
        &json!("refund please"),
        pipe_q.as_object().expect("map"),
        &pipe_cached,
        2,
    )
    .expect("second");
    assert_eq!(
        pipe_table.calls()[1],
        ["category\nrefund please".to_string()],
        "cache/predict repeat embeds query only"
    );
    assert_eq!(
        pipe_b["shortlist"]["intent"]["labels"],
        json!(["beta", "gamma"]),
        "cache/predict keeps shortlist metadata"
    );
    assert_eq!(
        pipe_b["answers"]["intent"]["choice"],
        json!("beta"),
        "cache/predict answer comes from the shortlist"
    );
}

// --------------------------------------------------------------- concurrent calls
#[test]
fn cache_concurrent_calls_return_correct_rows() {
    let mt_table = TableEmbed::new(cache_vectors());
    let mt_cached = Arc::new(cached_embed_fn(mt_table, 4096).expect("cached_embed_fn"));
    let texts_a = ["alpha".to_string(), "beta".to_string()];
    let texts_b = ["beta".to_string(), "gamma: mid".to_string()];
    let mut handles = Vec::new();
    for i in 0..16 {
        let cached = Arc::clone(&mt_cached);
        let texts = if i % 2 == 1 {
            texts_a.clone()
        } else {
            texts_b.clone()
        };
        handles.push(std::thread::spawn(move || {
            let rows = cached.embed(&texts).expect("embed");
            (texts.to_vec(), rows)
        }));
    }
    let results: Vec<(Vec<String>, Vec<Vec<f64>>)> = handles
        .into_iter()
        .map(|h| h.join().expect("join"))
        .collect();
    let expected: HashMap<String, Vec<f64>> = cache_vectors();
    for (texts, rows) in &results {
        let want: Vec<Vec<f64>> = texts.iter().map(|t| expected[t].clone()).collect();
        assert_eq!(rows, &want, "cache/concurrent calls return correct rows");
    }
    assert!(
        mt_cached.cache_info()["size"].as_u64().expect("size") <= 4096,
        "cache/concurrent size stays bounded"
    );
    let info = mt_cached.cache_info();
    let total = info["hits"].as_u64().expect("hits") + info["misses"].as_u64().expect("misses");
    assert_eq!(total, 32, "cache/concurrent counters consistent");
}
