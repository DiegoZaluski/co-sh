//! Ported tests: `tests/test_lang_stats.py` in full (the reference-impl
//! comparisons, with the expected values the port was pinned against CPython
//! on), the parity/complexity block of `tests/test_identifier_complexity.py`,
//! and the detector-level sections of `tests/test_router.py` (the
//! `is_english` table, `latin/undecided`, the `latin_lang` table, dotted
//! tokens #177, state flattening, dict-state #384 and the mixed-states
//! tables). The Router-level halves of those tables — the `.route(...).model`
//! assertions — are ported with the Router in Phase 6, as are the routing
//! halves of `test_lang_guess.py` / `test_blank_lang_routing.py`. The
//! upstream "analyse reports the same keys on every branch" check is enforced
//! structurally here: the `Analysis` struct cannot omit a field.

use serde_json::json;

use super::*;

/// `ref_counts` from `test_lang_stats.py`: the pre-refactor two-pass version,
/// kept verbatim so the single-pass port is pinned against real previous
/// behaviour rather than against hand-written expectations.
fn ref_counts(text: &str) -> Vec<(&'static str, usize)> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut latin = 0usize;
    for ch in text.chars() {
        if !py_is_alpha(ch) {
            continue;
        }
        let cp = ch as u32;
        if cp < 0x02B0
            || (0x1E00..=0x1EFF).contains(&cp)
            || (0xFF21..=0xFF3A).contains(&cp)
            || (0xFF41..=0xFF5A).contains(&cp)
        {
            latin += 1;
            continue;
        }
        let mut claimed = false;
        for (name, ranges) in SCRIPT_RANGES {
            if ranges.iter().any(|&(lo, hi)| lo <= cp && cp <= hi) {
                match counts.iter_mut().find(|(n, _)| n == name) {
                    Some((_, c)) => *c += 1,
                    None => counts.push((name, 1)),
                }
                claimed = true;
                break;
            }
        }
        if !claimed {
            match counts.iter_mut().find(|(n, _)| *n == "other") {
                Some((_, c)) => *c += 1,
                None => counts.push(("other", 1)),
            }
        }
    }
    counts.push(("latin", latin));
    counts
}

fn ref_detect(text: &str) -> &'static str {
    let counts = ref_counts(text);
    if counts.iter().all(|(_, c)| *c == 0) {
        return "unknown";
    }
    // Python's max returns the first of equal values; counts preserves
    // insertion order with "latin" last.
    let mut best = ("unknown", 0usize);
    for &(name, count) in &counts {
        if count > best.1 {
            best = (name, count);
        }
    }
    best.0
}

fn ref_profile(text: &str) -> Vec<(String, f64)> {
    let counts = ref_counts(text);
    let total: usize = counts.iter().map(|(_, c)| c).sum();
    if total == 0 {
        return Vec::new();
    }
    let mut ordered: Vec<(String, f64)> = Vec::new();
    let latin = counts.iter().find(|(n, _)| *n == "latin").unwrap().1;
    if latin > 0 {
        ordered.push(("latin".to_string(), latin as f64 / total as f64));
    }
    for (name, count) in &counts {
        if *name != "latin" && *count > 0 {
            ordered.push((name.to_string(), *count as f64 / total as f64));
        }
    }
    ordered
}

// ------------------------------------------------- test_lang_stats.py

const TEXTS: &[&str] = &[
    "",
    "   ",
    "hello world",
    "hello мир",
    "мир",
    "世界",
    "日本語です",
    "ΑΒΓΔ",
    "abc ᚠᚢᚦ", // an unlisted script lands under "other"
    "ab αβ",   // latin/greek tie
    "αβ аб",   // greek/cyrillic tie
    "ᚠᚢᚦabc", // other/latin tie
    "Hallo, meine Bestellung ist zweimal abgebucht worden.",
    "مرحبا كيف حالك",
    "İstanbul",
];

#[test]
fn detect_script_matches_the_reference_implementation() {
    for text in TEXTS {
        assert_eq!(detect_script(text), ref_detect(text), "detect_script({text:?})");
    }
}

#[test]
fn script_profile_matches_the_reference_implementation() {
    for text in TEXTS {
        assert_eq!(script_profile(text), ref_profile(text), "profile({text:?})");
    }
}

#[test]
fn analyse_profile_matches_the_reference_implementation() {
    for text in ["hello мир", "мир", "مرحبا كيف حالك", "hello world"] {
        assert_eq!(analyse(&json!(text)).script_profile, ref_profile(text));
    }
}

#[test]
fn analyse_script_anchors() {
    assert_eq!(analyse(&json!("مرحبا كيف حالك")).script, "arabic");
    assert_eq!(analyse(&json!("")).script, "unknown");
}

#[test]
fn a_per_script_fraction_vector_still_sums_to_one() {
    for text in ["hello мир", "abc ᚠᚢᚦ"] {
        let total: f64 = script_profile(text).iter().map(|(_, v)| *v).sum();
        assert_eq!((total * 1e9).round() / 1e9, 1.0);
    }
}

// CPython-pinned values for the same TEXTS: the tie-breaks and the "other"
// bucket are pinned outright, not only relatively.
#[test]
fn detect_script_values_pinned_against_cpython() {
    assert_eq!(detect_script(""), "unknown");
    assert_eq!(detect_script("   "), "unknown");
    assert_eq!(detect_script("hello world"), "latin");
    assert_eq!(detect_script("hello мир"), "latin");
    assert_eq!(detect_script("мир"), "cyrillic");
    assert_eq!(detect_script("世界"), "han");
    assert_eq!(detect_script("日本語です"), "han");
    assert_eq!(detect_script("ΑΒΓΔ"), "greek");
    assert_eq!(detect_script("abc ᚠᚢᚦ"), "other");
    assert_eq!(detect_script("ab αβ"), "greek");
    assert_eq!(detect_script("αβ аб"), "greek");
    assert_eq!(detect_script("ᚠᚢᚦabc"), "other");
    assert_eq!(detect_script("İstanbul"), "latin");
}

#[test]
fn script_profile_values_pinned_against_cpython() {
    // Python dict equality is order-insensitive; the profile keeps Latin
    // first when present, so compare as sets of pairs.
    let profile = |text: &str| -> std::collections::HashMap<String, f64> {
        script_profile(text).into_iter().collect()
    };
    let mut want = std::collections::HashMap::new();
    want.insert("latin".to_string(), 0.625);
    want.insert("cyrillic".to_string(), 0.375);
    assert_eq!(profile("hello мир"), want);
    let mut want = std::collections::HashMap::new();
    want.insert("han".to_string(), 0.6);
    want.insert("kana".to_string(), 0.4);
    assert_eq!(profile("日本語です"), want);
    let mut want = std::collections::HashMap::new();
    want.insert("latin".to_string(), 0.5);
    want.insert("greek".to_string(), 0.5);
    assert_eq!(profile("ab αβ"), want);
    let mut want = std::collections::HashMap::new();
    want.insert("greek".to_string(), 0.5);
    want.insert("cyrillic".to_string(), 0.5);
    assert_eq!(profile("αβ аб"), want);
    let mut want = std::collections::HashMap::new();
    want.insert("other".to_string(), 0.5);
    want.insert("latin".to_string(), 0.5);
    assert_eq!(profile("abc ᚠᚢᚦ"), want);
    assert!(profile("").is_empty());
    // ... but the public order is still pinned: Latin first when present.
    assert_eq!(script_profile("abc ᚠᚢᚦ")[0].0, "latin");
    assert_eq!(script_profile("αβ аб")[0].0, "greek");
}

// --------------------------------------- test_identifier_complexity.py

/// Every identifier the `_IDENTIFIER` pattern exists to strip, plus the shapes
/// that tempt a bounded-quantifier fix into leaving a residue token behind.
/// Expected outputs pinned against CPython (`_IDENTIFIER.sub(" ", token)`).
const IDENTIFIER_PARITY: &[(&str, &str)] = &[
    ("github.com", " "),
    ("user@acme.com", " "),
    ("v1.2.3", " "),
    ("U.S.A.", " ."),
    ("arrivato.", "arrivato."),
    ("a.b", " "),
    ("a@b", " "),
    ("sub.domain.co.uk", " "),
    ("first.last@sub.example.org", " "),
    ("192.168.0.1", " "),
    ("-a.b-", " "),
    ("_x.y_", " "),
    ("foo..bar", "foo. "),
    (".com", " "),
    ("a.", "a."),
    ("@a", " "),
    ("a@", "a@"),
    ("e.g.", " ."),
    ("Ü.Ö", " "),
    ("naïve.café", " "),
    ("a-b.c-d", " "),
    ("x-a.b", " "),
    ("9.9", " "),
    ("a..b", "a. "),
    ("a.-b", " "),
    ("a-.b-.c", " "),
    ("", ""),
    (".", "."),
    ("@", "@"),
    (
        "Contact support@acme.com or github.com/acme for v2.10.1 details.",
        "Contact   or  /acme for   details.",
    ),
    ("No identifiers here at all just words", "No identifiers here at all just words"),
];

#[test]
fn identifier_stripping_matches_the_python_pattern() {
    for (token, want) in IDENTIFIER_PARITY {
        assert_eq!(strip_identifiers(token), *want, "parity/{token:?}");
    }
    // A local part at RFC 5321's 64-character limit, and a run past any
    // plausible bound: a length-bounded pattern strips only part of these and
    // leaves the rest as a word.
    assert_eq!(strip_identifiers(&format!("{}@example.com", "a".repeat(64))), " ");
    assert_eq!(strip_identifiers(&format!("{}.example.com", "a".repeat(200))), " ");
    assert_eq!(
        strip_identifiers(&format!("grazie mille per {}wzqxk.example.com", "wzqxk".repeat(15))),
        "grazie mille per  "
    );
}

#[test]
fn identifier_runs_of_pure_word_characters_stay_linear() {
    // Ceilings from the upstream test (its previous-pattern costs were
    // measured on a 2023 laptop): the scanner is linear by construction, so
    // the port stays far under them even in a debug build.
    for (text, ceiling) in [
        ("a".repeat(4_000), 0.05),
        ("a".repeat(20_000), 1.0),
        ("a-".repeat(10_000), 1.0),
        ("a_".repeat(10_000), 1.0),
        (format!("{}.", "a".repeat(20_000)), 1.0),
    ] {
        let started = std::time::Instant::now();
        let _ = strip_identifiers(&text);
        let took = started.elapsed();
        assert!(
            took.as_secs_f64() < ceiling,
            "{} characters stripped in {took:?}",
            text.chars().count()
        );
    }
}

#[test]
fn one_long_token_costs_about_what_prose_costs() {
    // The public entry point a request actually reaches. Compared against
    // prose of the same size: the ratio is ~1x when the work is linear and
    // ~100x when it is not.
    let prose: String = "The customer was billed twice and wants a refund. "
        .repeat(80)
        .chars()
        .take(4_000)
        .collect();
    let best = |state: &Value| {
        (0..3)
            .map(|_| {
                let started = std::time::Instant::now();
                let _ = analyse(state);
                started.elapsed()
            })
            .min()
            .unwrap()
            .as_secs_f64()
    };
    let token_cost = best(&json!("a".repeat(4_000)));
    let prose_cost = best(&json!(prose)).max(1e-5);
    let ratio = token_cost / prose_cost;
    assert!(ratio < 10.0, "one long token costs {ratio:.1}x prose");
}

// --------------------- detector-level sections of tests/test_router.py

#[test]
fn the_is_english_table() {
    let check = |text: &str, want: bool| assert_eq!(is_english(&json!(text)), want, "{text:?}");
    check("Please refund the duplicate charge on invoice 4411 today.", true);
    check("Հայերեն", false);
    check(
        "Sifarisim gelmedi ve pulum geri qaytarilmadi, zehmet olmasa yoxlayin",
        false,
    );
    check(
        "Mən sizin xidmətinizdən razı deyiləm və pulumu geri istəyirəm",
        false,
    );
    check("refund me", true);
    check("ग्राहक से दो बार शुल्क लिया गया", false);
    check("お客様は二重に請求されました", false);
    check("С клиента дважды сняли деньги", false);
    check(
        "Le client a été facturé deux fois et il demande un remboursement pour la \
         facture qui a été payée le mois dernier avec la carte de crédit",
        false,
    );
    check(
        "Der Kunde wurde zweimal belastet und möchte eine Rückerstattung für die \
         Rechnung die nicht korrekt ist und auch nicht bezahlt wurde",
        false,
    );
    // Latin-script languages with no stopword list of their own: reported in
    // #35, where Romanian states were handed to the English checkpoint (0.330
    // accuracy, 0.658 ECE on `ro`). An unidentified language must never be
    // assumed English.
    check("Gătește-mi o rețetă de sarmale de post pentru mâine.", false);
    check(
        "Am fost taxat de două ori pentru factura din luna martie și vreau banii",
        false,
    );
    check("Klient został obciążony dwukrotnie i chce zwrot pieniędzy za fakturę", false);
    check("Zákazníkovi byla částka účtována dvakrát a žádá o vrácení peněz", false);
    check("Müşteriden iki kez ücret alındı ve para iadesi istiyor lütfen yardım", false);
    check(
        "Khách hàng đã bị thu phí hai lần và muốn được hoàn tiền ngay",
        false,
    );
    // English with the odd loanword must not tip over into the multilingual
    // checkpoint
    check(
        "We visited a cafe in Zurich and the naive assumption about the invoice \
         was wrong, so please refund the duplicate charge",
        true,
    );
}

#[test]
fn undecided_is_reported_as_undecided() {
    // a single shared function word used to name a language ("para" in
    // Turkish text was called Spanish)
    let turkish = "Müşteriden iki kez ücret alındı ve para iadesi istiyor";
    assert!(analyse(&json!(turkish)).language_undecided);
    assert_eq!(analyse(&json!(turkish)).language, None);
    assert!(
        !analyse(&json!("Please refund the duplicate charge on the invoice")).language_undecided
    );
    assert!(analyse(&json!("Gătește-mi o rețetă de sarmale")).diacritic_rate > 0.02);
    assert_eq!(analyse(&json!("Please refund the duplicate charge today")).diacritic_rate, 0.0);
}

#[test]
fn a_zero_tie_between_stopword_lists_invents_nothing() {
    assert_eq!(guess_latin_language("Cât e ora acum la Tokyo"), None);
}

#[test]
fn the_known_romanian_gap_is_kept_visible() {
    // Known limitation, kept visible on purpose upstream: Romanian short
    // enough to carry no diacritics and an English function word ("in")
    // still reads as English. A real LID model is the fix, not more
    // stopwords (see the discussion in #35).
    assert!(is_english(&json!("Care este ora in Tokyo?")));
}

#[test]
fn the_latin_lang_table() {
    let check = |text: &str, want: Option<&str>| {
        assert_eq!(guess_latin_language(text).as_deref(), want, "{text:?}")
    };
    check("The customer was charged twice and wants a refund for this invoice", Some("en"));
    check(
        "Le client a ete facture deux fois et il demande un remboursement pour la facture",
        Some("fr"),
    );
    check(
        "Der Kunde wurde zweimal belastet und moechte eine Rueckerstattung fuer die Rechnung",
        Some("de"),
    );
    check(
        "El cliente fue cobrado dos veces y quiere que le devuelvan el dinero por la factura",
        Some("es"),
    );
    check("Zəhmət olmasa, sifarişim üçün pulu geri qaytarın, çünki məhsul gəlmədi", Some("az"));
    // one shared function word is not enough to name a language
    check("Müştəridən iki dəfə pul alınıb və o, geri qaytarılmasını istəyir", None);
    check("MÜŞTƏRİ İLƏ ƏLAQƏ SAXLAYIN VƏ PULU GERİ QAYTARIN", Some("az"));
    check("refund", None);
    check(
        "Please refund the duplicate charge on invoice 4411 today because we have \
         been waiting for three days and nobody has replied to us",
        Some("en"),
    );
}

#[test]
fn dotted_tokens_are_identifiers_not_prose() {
    // `com` is Portuguese ("with") and `o` its article, and `_WORD` splits
    // `github.com` into `github` + `com`: two domains were enough to cross
    // the margin and send an English state to the multilingual checkpoint,
    // reported as Portuguese (#177).
    let url_email = json!({"url": "github.com", "email": "user@acme.com"});
    assert_eq!(analyse(&url_email).language, None);
    assert!(is_english(&url_email));
    assert_eq!(analyse(&json!("github.com acme.com")).language, None);
    assert!(is_english(&json!("github.com acme.com")));
    let links = json!({"links": ["example.com", "example.co.uk", "docs.readthedocs.io"]});
    assert_eq!(analyse(&links).language, None);
    assert!(is_english(&links));
    // versions, decimals and dotted abbreviations are identifiers too, and
    // were never prose
    assert!(is_english(&json!("build 1.2.3 on 12.30 with ratio 0.5")));
    assert!(is_english(&json!("Report by Smith et al., e.g. the U.S.A. office")));
    // masking identifiers must not cost the prose around them its language,
    // and a full stop ends a sentence rather than joining an identifier: the
    // word before it keeps its letters
    assert_eq!(
        guess_latin_language(
            "O cliente nao recebeu o produto, mas quer o dinheiro para a conta, veja example.com"
        )
        .as_deref(),
        Some("pt")
    );
    assert_eq!(
        guess_latin_language("O cliente nao recebeu o produto, mas quer o dinheiro para a conta.")
            .as_deref(),
        Some("pt")
    );
}

#[test]
fn state_flattening() {
    assert!(
        state_text(&json!({"body": "charged twice", "n": 3}), 4000).contains("charged twice")
    );
    assert!(state_text(&json!({"a": {"b": ["deep"]}}), 4000).contains("deep"));
    assert!(state_text(&json!(["x", {"y": "z"}]), 4000).contains("x"));
    assert_eq!(state_text(&Value::Null, 4000), "");
    // keys must not drive detection: English keys around Hindi content stay
    // non-English
    assert!(
        !analyse(&json!({"subject": "नमस्ते", "body": "ग्राहक से दो बार शुल्क लिया गया"}))
            .is_english
    );
}

#[test]
fn dict_state_language_matches_string_state() {
    // The same German sentence must read the same way as a string and as a
    // dict value (#384).
    let de = "Mein Konto wurde zweimal belastet";
    assert_eq!(
        analyse(&json!({"message": de})).language,
        analyse(&json!(de)).language
    );
}

// ---------------------------------------------------------------- mixed states
// A Portuguese ticket carrying an English stack trace, error payload or form
// template reads as English as a whole — the English part is longer — and
// went to the checkpoint that cannot read the customer's own words (#384).
// Any line or field that on its own is named a non-English language now wins.
const TRACE: &str = "O sistema caiu de novo hoje de manhã, segue o log:\n\
                     Traceback (most recent call last):\n\
                     \x20 File \"/app/main.py\", line 42, in handler\n\
                     \x20   return self.process(request)\n\
                     ConnectionError: the connection to the database was refused because the \
                     pool is exhausted and there is no available slot for this request";

#[test]
fn mixed_states_find_the_foreign_line() {
    for (state, segment) in [
        (json!(TRACE), "O sistema caiu de novo hoje de manhã, segue o log:"),
        (
            json!({"descricao": "O pagamento não foi processado",
                   "error": {"code": "card_declined",
                             "message": "Your card was declined. Please try again with a \
                                         different card or contact your bank for more information."}}),
            "O pagamento não foi processado",
        ),
        (
            json!({"subject": "New ticket from the web form", "body": "Quero cancelar meu plano"}),
            "Quero cancelar meu plano",
        ),
        (
            json!({"subject": "Urgent: production is down for all customers since the last \
                               deploy and the status page is red for the whole region",
                   "body": "Deu erro (500) no login, alguém pode ver isso agora?"}),
            "Deu erro (500) no login, alguém pode ver isso agora?",
        ),
        // the rule runs both ways: an English ticket that pastes a foreign
        // log goes to multilingual too
        (
            json!("Our Brazilian branch cannot issue invoices since this morning. The system \
                   shows this message:\nERRO: Não foi possível emitir a nota fiscal, o \
                   certificado digital está vencido\nCan you help us before the end of the day?"),
            "ERRO: Não foi possível emitir a nota fiscal, o certificado digital está vencido",
        ),
        (
            json!("The nightly sync to the Munich server keeps failing and we lose the whole \
                   batch.\nFehler: Die Verbindung zum Server wurde unterbrochen, bitte \
                   versuchen Sie es spaeter noch einmal\nPlease check the firewall rules on \
                   your side."),
            "Fehler: Die Verbindung zum Server wurde unterbrochen, bitte versuchen Sie es \
             spaeter noch einmal",
        ),
        (
            json!({"subject": "Payment failed for a customer in Madrid",
                   "description": "The customer tried three times with the same card and each \
                                   attempt was declined by the gateway, so we would like to know \
                                   whether the problem is on our side or with the bank.",
                   "error": {"code": "card_declined",
                             "message": "La tarjeta fue rechazada por el banco emisor, contacte \
                                         con su banco"}}),
            "La tarjeta fue rechazada por el banco emisor, contacte con su banco",
        ),
        // acronyms are dropped only from mixed-case text: a line written all
        // in capitals keeps its words
        (
            json!("This is the fourth email I have sent about the same order and nobody has \
                   answered any of them.\nThe customer wrote this in the chat and then closed \
                   the window:\nQUERO MEU DINHEIRO DE VOLTA AGORA\nCould someone from the \
                   billing team look at order 5512 today?"),
            "QUERO MEU DINHEIRO DE VOLTA AGORA",
        ),
    ] {
        assert!(!is_english(&state), "mixed/is not english: {segment:?}");
        assert_eq!(
            analyse(&state).mixed_segment.as_deref(),
            Some(segment),
            "mixed/segment reported: {segment:?}"
        );
    }
}

#[test]
fn english_states_stay_english() {
    // English stays English: several English lines, a short foreign
    // sign-off, and code pasted into a request (`os.path` reads as
    // Portuguese, `round(el, 2)` as Spanish, `np.mean(na)` as Portuguese).
    for state in [
        json!("Hi team,\nThe export failed again last night.\nCan you check the logs?\nThanks"),
        json!("Please resend the invoice for March, the amount is wrong.\nAtenciosamente, Joao"),
        json!("The build broke after the refactor.\nREPO = os.path.dirname(os.path.dirname(__file__))\nPlease take a look at the import paths when you can."),
        json!("The latency script crashes on large runs.\nmix[key] = {\"total_s\": round(el, 2)}\nCan you check why the stream is empty?"),
        json!("The summary is wrong for empty suites.\nif na: non[m] = round(float(np.mean(na)), 4)\nPlease guard the empty case."),
        json!({"status": "open", "priority": "high",
               "message": "The customer was charged twice and wants a refund"}),
        // a line carries far less text than a state, so its evidence must be
        // two different words and no acronyms or slash compounds. A
        // ham-radio listing on 20 Newsgroups went to multilingual on
        // `COM ... COM` alone; hockey picks on the team codes, OS/2 on `os`
        // and `dos`.
        json!("I'm looking for good deals on the following (used or new):\nAviation Headsets (with mic).\nHandheld Nav/Com tranciever (may consider COM only).\nPortable GPS or Loran Navigator."),
        json!("Round two predictions for the pool, as promised.\nQUE  vs MON:  MON  in 7.\nPIT  vs NYI:  PIT  in 5."),
        json!("I need a converter for these image formats.\nDOS, OS/2 or platform independent programs if possible.\nThanks in advance."),
        json!("My modem stopped answering after the upgrade.\nC:\\DOS\\mode COM1:9600,n,8,1,p\nIs that the right line for a 9600 baud connection?"),
    ] {
        assert!(is_english(&state), "mixed/english stays english");
        assert_eq!(analyse(&state).mixed_segment, None, "mixed/no segment");
    }
}

#[test]
fn the_segment_check_reads_at_most_the_cap() {
    // a state is user input: one long line with no joiner took 43 s at 40,000
    // characters when compounds were stripped with an open-ended regex; the
    // segment check now reads at most the 4,000-character cap
    let started = std::time::Instant::now();
    analyse(&json!({
        "subject": "The export failed again last night for the whole region",
        "body": "a".repeat(200_000)
    }));
    assert!(started.elapsed().as_secs_f64() < 5.0, "long single-line field stays fast");
    // a value the segment scan did not reach is still read on its own
    // afterwards (#384): the long log fills the cap, the body is next
    assert_eq!(
        analyse(&json!({
            "log": "The export failed again last night for the whole region. ".repeat(80),
            "body": "Quero cancelar meu plano agora mesmo"
        }))
        .mixed_segment
        .as_deref(),
        None
    );
}

// ------------------------------------------------- latin_profile anchors

#[test]
fn latin_profile_values_pinned_against_cpython() {
    let p = |text| latin_profile(text);
    assert_eq!(p("hello world").language, None);
    assert_eq!(p("hello world").english_hits, 0);
    assert_eq!(p("").language, None);
    assert_eq!(p("ab").language, None);
    // undecided and not looking non-English: no hits, under the word floor
    let undecided = p("je voudrais un cafe");
    assert_eq!(undecided.language, None);
    assert!(!undecided.looks_non_english);
}

#[test]
fn classifier_semantics_pinned_against_python() {
    // str.isalpha: L* only — Nl roman numerals and Mn marks are not alpha
    assert!(py_is_alpha('a') && py_is_alpha('é') && py_is_alpha('ǅ') && py_is_alpha('世'));
    assert!(!py_is_alpha('Ⅰ') && !py_is_alpha('ा') && !py_is_alpha('½'));
    // str.isupper / str.islower: the Uppercase/Lowercase properties
    assert!(py_is_upper('Ⅰ'));
    assert!(py_is_lower('ⅱ'));
    assert!(!py_is_upper('ǅ') && !py_is_lower('ǅ'));
    // unicodedata.combining
    assert!(py_is_combining('\u{0301}'));
    assert!(!py_is_combining('a'));
}
