use super::*;

// --------------------------------------------------------------- cosine (test_shortlist_cosine.py)
#[test]
fn cosine_identical_vectors() {
    let v = [1.0, 2.0, 3.0];
    let docs = [vec![1.0, 2.0, 3.0], vec![-1.0, -2.0, -3.0]];
    let sims = cosine(&v, &docs);
    assert!(
        (sims[0] - 1.0).abs() < 1e-5,
        "test_cosine_identical_vectors +1"
    );
    assert!(
        (sims[1] - (-1.0)).abs() < 1e-5,
        "test_cosine_identical_vectors -1"
    );
}

#[test]
fn cosine_zero_vectors() {
    let v = [0.0; 4];
    let docs = [vec![1.0; 4], vec![1.0; 4], vec![1.0; 4]];
    let sims = cosine(&v, &docs);
    assert!(sims.iter().all(|s| *s == 0.0), "test_cosine_zero_vectors");
}

#[test]
fn cosine_empty_docs() {
    let v = [1.0; 4];
    let docs: [Vec<f64>; 0] = [];
    let sims = cosine(&v, &docs);
    assert!(sims.is_empty(), "test_cosine_empty_docs");
}
