# Language detection: evidence for routing

Automatic routing needs one conservative answer: can the English checkpoint
read this state, or should the multilingual checkpoint handle it? The
`lang` module supplies that evidence without a second model.

## Script comes first

`detect_script` identifies the strongest script in text, while
`script_profile` returns the fractions for every script it sees. Non-Latin
scripts are strong evidence for the multilingual checkpoint. A Latin script
is only the beginning of the decision; it does not prove English.

```rust
use cosh_onnx::lang::{detect_script, latin_profile, script_profile};

assert_eq!(detect_script("Hello from Rust"), "latin");
let profile = script_profile("Olá, mundo");
assert!(!profile.is_empty());

let latin = latin_profile("The service is running and the request is ready");
assert_eq!(latin.language.as_deref(), Some("en"));
```

## Latin language guesses are best effort

`latin_profile` uses stopwords, diacritics and identifier-aware word runs. It
returns `None` for short or ambiguous text on purpose. `guess_latin_language`
is a convenience accessor when the full evidence is not needed.

Do not treat this heuristic as a translation service. If an application knows
the language, pass `PredictOptions::lang` explicitly. If it has its own
detector, use `LangGuess::Callable` or configure the router with a language
hint.

## Structured state is flattened consistently

`analyse` accepts a JSON state and returns `Analysis`: script, script profile,
optional language, English flag, undecided flag, diacritic rate,
non-Latin fraction and a mixed segment. The router uses the same flattening
for string and object states, so changing the transport shape does not change
the detection result.

## Conservative defaults

When evidence is insufficient, the router uses its configured `default`.
That makes deployment policy explicit: an English-first application can keep
the default, while a multilingual service can configure the opposite without
pretending that a short message was confidently classified.

## Summary

- Script detection is the strongest signal for non-Latin input.
- Latin language recognition is a best-effort heuristic and may abstain.
- Explicit language hints override detection.
- Structured and string states use the same analysis path.
- The router's default is a policy choice for undecidable text.
