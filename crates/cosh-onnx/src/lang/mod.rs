//! Dependency-free language/script detection used to route between the
//! Laya checkpoints.
//!
//! Routing only needs one decision: *is this English Latin text, or is it
//! something the English checkpoint cannot read?* Benchmarks on MASSIVE (14
//! languages) showed the English checkpoint collapsing to near-random on
//! non-Latin scripts (Hindi 0.100, Korean 0.103, Swahili 0.103, Tamil 0.113 at
//! 20 options, where random is 0.050), while holding up far better on
//! Latin-script languages (French 0.487, Spanish 0.480). So the signal that
//! matters most is *script*, and the secondary signal is whether Latin text is
//! English.
//!
//! Script detection is exact. The Latin-script language guess is a
//! stopword/diacritic heuristic and is explicitly best-effort: pass an
//! explicit model or `lang=` when you already know the language.
//!
//! # Documented divergences (read from upstream, not assumed)
//!
//! - Python drives detection with the `re` patterns `_WORD`, `_IDENTIFIER`,
//!   `_CODE_LINE`, `_JOINED` and `_LETTER_RUN`, whose `\w` is
//!   `str.isalnum() + '_'`. This port replaces them with linear scanners over
//!   `char`s classified through `icu_properties` (`isalnum` = general category
//!   L* ∪ Nd/Nl/No; per-char `isupper`/`islower` = the Uppercase/Lowercase
//!   binary properties), verified sweep-wide against CPython — this also
//!   satisfies `test_identifier_complexity.py`'s linearity requirement by
//!   construction.
//! - `state_text` caps at `max_chars` with Python character semantics; Rust
//!   `char`s match that exactly.
//! - The process-wide caches of upstream (`_SHARED_WORDS` /
//!   `_EN_ONLY_WORDS` set comprehensions) are precomputed as `OnceLock`s over
//!   the same static tables.
//! - `analyse`'s `script_profile` dict keeps Latin first when present; the
//!   Rust profile uses a `Vec<(String, f64)>` to preserve that order exactly.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::Value;

use crate::pycompat::{py_round, round4};

/// `_SCRIPT_RANGES`: Unicode blocks the English checkpoint cannot read
/// (name, inclusive ranges), verbatim from `lang.py`.
static SCRIPT_RANGES: &[(&str, &[(u32, u32)])] = &[
    ("greek", &[(0x0370, 0x03FF), (0x1F00, 0x1FFF)]),
    ("cyrillic", &[(0x0400, 0x052F), (0x2DE0, 0x2DFF), (0xA640, 0xA69F)]),
    ("armenian", &[(0x0530, 0x058F)]),
    ("hebrew", &[(0x0590, 0x05FF)]),
    ("arabic", &[(0x0600, 0x06FF), (0x0750, 0x077F), (0x08A0, 0x08FF), (0xFB50, 0xFDFF), (0xFE70, 0xFEFF)]),
    ("devanagari", &[(0x0900, 0x097F), (0xA8E0, 0xA8FF)]),
    ("bengali", &[(0x0980, 0x09FF)]),
    ("gurmukhi", &[(0x0A00, 0x0A7F)]),
    ("gujarati", &[(0x0A80, 0x0AFF)]),
    ("oriya", &[(0x0B00, 0x0B7F)]),
    ("tamil", &[(0x0B80, 0x0BFF)]),
    ("telugu", &[(0x0C00, 0x0C7F)]),
    ("kannada", &[(0x0C80, 0x0CFF)]),
    ("malayalam", &[(0x0D00, 0x0D7F)]),
    ("sinhala", &[(0x0D80, 0x0DFF)]),
    ("thai", &[(0x0E00, 0x0E7F)]),
    ("lao", &[(0x0E80, 0x0EFF)]),
    ("tibetan", &[(0x0F00, 0x0FFF)]),
    ("myanmar", &[(0x1000, 0x109F)]),
    ("georgian", &[(0x10A0, 0x10FF)]),
    ("ethiopic", &[(0x1200, 0x137F)]),
    ("khmer", &[(0x1780, 0x17FF)]),
    ("hangul", &[(0x1100, 0x11FF), (0x3130, 0x318F), (0xAC00, 0xD7AF)]),
    ("kana", &[(0x3040, 0x309F), (0x30A0, 0x30FF), (0x31F0, 0x31FF)]),
    ("han", &[(0x3400, 0x4DBF), (0x4E00, 0x9FFF), (0xF900, 0xFAFF)]),
];

/// `_STOP`: function words per language, verbatim from `lang.py`
/// (same key order; the order decides the tie-break in `latin_profile`).
static STOP_LANGUAGES: &[(&str, &[&str])] = &[
    ("en", &["and", "are", "as", "at", "be", "but", "can", "for", "from", "has", "have", "i", "in", "is", "it", "not", "of", "on", "please", "that", "the", "their", "there", "this", "to", "was", "we", "were", "what", "which", "will", "with", "would", "you"]),
    ("fr", &["alors", "au", "aux", "avec", "bien", "bonjour", "ce", "ces", "cette", "comment", "dans", "des", "deux", "dois", "doit", "donc", "du", "elle", "elles", "est", "et", "fait", "fois", "il", "ils", "je", "jour", "jours", "la", "le", "les", "ma", "mais", "merci", "mes", "mois", "mon", "nous", "ont", "ou", "pas", "peut", "peux", "plus", "pour", "pourquoi", "quand", "que", "qui", "sa", "ses", "sont", "sur", "ta", "tes", "ton", "tous", "tout", "toute", "trois", "très", "tu", "une", "veut", "veux", "vous", "être"]),
    ("de", &["aber", "auch", "auf", "aus", "bei", "bitte", "das", "dem", "den", "der", "dich", "die", "diese", "diesen", "dieser", "dieses", "dir", "ein", "eine", "einem", "einen", "einer", "für", "gibt", "habe", "haben", "heute", "ich", "im", "in", "ist", "jetzt", "kann", "kannst", "mein", "meine", "meinem", "meinen", "meiner", "mich", "mir", "mit", "nach", "nicht", "noch", "oder", "sich", "sind", "und", "uns", "von", "wann", "was", "welche", "werden", "wie", "wir", "wird", "wo", "wurde", "zu", "zum", "zur"]),
    ("es", &["al", "algo", "aquí", "aunque", "como", "con", "cuando", "del", "donde", "dos", "el", "entre", "es", "esa", "ese", "eso", "esta", "este", "esto", "está", "fue", "fueron", "gracias", "han", "hay", "hemos", "hoy", "la", "las", "le", "les", "lo", "los", "mi", "muy", "más", "nada", "necesito", "ni", "nos", "para", "pero", "por", "porque", "puede", "pueden", "que", "quiero", "se", "ser", "sobre", "son", "su", "sus", "también", "tengo", "tiene", "tienen", "todo", "tres", "tu", "un", "una", "y", "ya"]),
    ("pt", &["agora", "ainda", "alguem", "alguém", "ali", "antes", "ao", "aos", "aqui", "as", "até", "boa", "cadê", "com", "como", "consigo", "da", "das", "depois", "deu", "do", "dois", "dos", "e", "em", "entao", "então", "era", "esta", "estamos", "estava", "este", "estou", "está", "eu", "ficou", "fiz", "foi", "gostaria", "hoje", "isso", "isto", "ja", "já", "mais", "mas", "meu", "meus", "minha", "minhas", "muito", "na", "nada", "nao", "nas", "nenhum", "nenhuma", "ninguem", "ninguém", "noite", "nos", "nossa", "nosso", "não", "o", "obrigada", "obrigado", "olá", "onde", "ontem", "os", "para", "pela", "pelo", "pode", "podem", "por", "porque", "pra", "preciso", "quando", "que", "quero", "sao", "se", "ser", "seu", "sou", "sua", "são", "tambem", "também", "tarde", "tem", "tenho", "três", "tudo", "tá", "um", "uma", "vc", "vcs", "voce", "voces", "você", "vocês", "é"]),
    ("it", &["abbiamo", "adesso", "agli", "alla", "alle", "anche", "ancora", "avete", "che", "ci", "ciao", "col", "come", "con", "da", "dagli", "dal", "dalla", "dallo", "degli", "dei", "del", "della", "delle", "dello", "deve", "devo", "devono", "di", "dove", "e", "ed", "era", "fra", "già", "gli", "grazie", "ha", "hai", "hanno", "ho", "ieri", "il", "la", "le", "lo", "mai", "mi", "mia", "mio", "molto", "ne", "negli", "nel", "nell", "nella", "non", "o", "oggi", "per", "perche", "più", "poco", "quando", "questa", "questo", "scusa", "sempre", "si", "sono", "stata", "stato", "su", "sua", "sul", "sulla", "sulle", "tra", "tuo", "un", "una", "uno", "voglio", "vorrei", "è"]),
    ("nl", &["aan", "dat", "deze", "door", "een", "het", "is", "maar", "met", "naar", "niet", "ook", "op", "te", "van", "voor", "worden", "wordt", "zijn"]),
    ("ro", &["aceasta", "această", "acest", "acesta", "acum", "ale", "care", "dar", "din", "după", "este", "foarte", "fost", "fără", "lui", "mi", "nu", "pentru", "până", "sunt", "să", "trebuie", "vreau", "vă", "în", "și", "ți"]),
    ("bn", &["abar", "ajke", "akhon", "amader", "amake", "amar", "ami", "amra", "apnake", "apnar", "apnara", "apni", "ar", "asbe", "bhai", "bhalo", "bolte", "bolun", "chai", "chaina", "dhonnobad", "dilam", "dite", "diye", "diyechi", "dorkar", "duibar", "ei", "eita", "ekbar", "ekhon", "ekhono", "ekta", "ferot", "geche", "gese", "hobe", "hocche", "hoise", "hoye", "hoyeche", "hoyni", "jabe", "jodi", "jonno", "kalke", "keno", "keu", "kharap", "khub", "ki", "kibhabe", "kichu", "kintu", "kivabe", "kobe", "kokhon", "korbo", "korchi", "kore", "koreche", "korechi", "koren", "korlam", "korsi", "korte", "korun", "kothay", "koto", "lagbe", "moddhe", "nai", "niye", "oi", "oita", "onek", "ota", "pabo", "paini", "parben", "parbo", "parchi", "parchina", "peyechi", "sathe", "shathe", "shob", "shomossa", "somossa", "tader", "tahole", "taka", "theke", "tomake", "tomar", "tomra", "tumi", "valo"]),
    ("az", &["amma", "ancaq", "artiq", "artıq", "bir", "biz", "bu", "cox", "daha", "deyil", "də", "eger", "görə", "həm", "hər", "ile", "ilə", "isə", "kimi", "lakin", "mən", "nə", "olan", "olmasa", "olub", "onlar", "siz", "sonra", "sən", "ucun", "var", "ve", "və", "yalniz", "yalnız", "yox", "yoxdur", "çox", "üçün", "əgər"]),
];

/// `_NON_EN_DIACRITICS`: letters ordinary English does not use.
static NON_EN_DIACRITICS: &str = "ßàáâãäåæçèéêëìíîïñòóôõöøùúûüýÿāăąćčďđēęěğģīıķļłńņňőœřśşšţťūůűźżžșțə";

/// A diacritic rate above this is taken as evidence the text is not English,
/// even when no stopword list matches it.
pub const NON_EN_DIACRITIC_RATE: f64 = 0.02;

/// One accented loanword or proper noun (`café`, `résumé`, `José`) must not
/// alone pull otherwise plain English off the English checkpoint: the rate is
/// measured over every character, so a single `é` in a short sentence clears
/// the floor above. English function words keep their say only through the
/// rescue below — two distinct words no other list holds, at most one
/// non-English letter word, and a rate still well above the floor vetoes
/// regardless.
pub const ENGLISH_RESCUE_DIACRITIC_RATE: f64 = 0.06;

/// Non-Latin text is not for the English checkpoint even when Latin letters
/// are the plurality: a brand name or order code outvotes the CJK request
/// around it letter for letter, though one CJK character carries far more than
/// a letter. A short message needs a large share to count; a long payload
/// (ticket fields, English agent turns) dilutes the share, so there a
/// sentence's worth of letters counts too.
pub const NON_LATIN_FRACTION: f64 = 0.2;
pub const NON_LATIN_MIN_FRACTION: f64 = 0.1;
pub const NON_LATIN_MIN_LETTERS: usize = 10;

// ------------------------------------------------------------------ Unicode
//
// The classifier semantics of Python's `str` methods, pinned by a sweep over
// the whole codepoint space against CPython:
// - `isalpha`  == general category Lu|Ll|Lt|Lm|Lo
// - `isalnum`  == isalpha | Nd | Nl | No
// - `isupper`  == the Unicode `Uppercase` property (Nl roman numerals included)
// - `islower`  == the Unicode `Lowercase` property (Lm modifier letters included)
// - combining  == canonical combining class != 0 (a `char` is always one
//                 codepoint, so the multi-char behaviour of `unicodedata`
//                 never applies here).
//
// The ICU handles below borrow compiled-in data, so they are memoized once:
// detection walks every character, and `_IDENTIFIER`'s complexity test pins
// linear behaviour on 20 000-character runs.

fn general_category() -> icu_properties::CodePointMapDataBorrowed<
    'static,
    icu_properties::props::GeneralCategory,
> {
    static CELL: std::sync::OnceLock<
        icu_properties::CodePointMapDataBorrowed<'static, icu_properties::props::GeneralCategory>,
    > = std::sync::OnceLock::new();
    *CELL.get_or_init(icu_properties::CodePointMapData::new)
}

fn combining_class()
-> icu_properties::CodePointMapDataBorrowed<'static, icu_properties::props::CanonicalCombiningClass>
{
    static CELL: std::sync::OnceLock<
        icu_properties::CodePointMapDataBorrowed<
            'static,
            icu_properties::props::CanonicalCombiningClass,
        >,
    > = std::sync::OnceLock::new();
    *CELL.get_or_init(icu_properties::CodePointMapData::new)
}

/// `str.isalpha()` (general category L*).
pub(crate) fn py_is_alpha(ch: char) -> bool {
    use icu_properties::props::GeneralCategory;
    matches!(
        general_category().get(ch),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

/// `str.isalnum()` (L* plus the numeric letters and numbers).
pub(crate) fn py_is_alnum(ch: char) -> bool {
    use icu_properties::props::GeneralCategory;
    matches!(
        general_category().get(ch),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
    )
}

/// `str.isupper()` on a single character (the `Uppercase` binary property).
pub(crate) fn py_is_upper(ch: char) -> bool {
    static CELL: std::sync::OnceLock<icu_properties::CodePointSetDataBorrowed<'static>> =
        std::sync::OnceLock::new();
    CELL.get_or_init(icu_properties::CodePointSetData::new::<icu_properties::props::Uppercase>)
        .contains(ch)
}

/// `str.islower()` on a single character (the `Lowercase` binary property).
pub(crate) fn py_is_lower(ch: char) -> bool {
    static CELL: std::sync::OnceLock<icu_properties::CodePointSetDataBorrowed<'static>> =
        std::sync::OnceLock::new();
    CELL.get_or_init(icu_properties::CodePointSetData::new::<icu_properties::props::Lowercase>)
        .contains(ch)
}

/// `unicodedata.combining(ch) != 0` (the canonical combining class).
pub(crate) fn py_is_combining(ch: char) -> bool {
    combining_class().get(ch).to_icu4c_value() != 0
}

/// `_script_of` upstream is `def _script_of(ch)`; the port takes the
/// codepoint directly so both `script_counts` and `non_latin_words` can share
/// it. The named non-Latin script of one letter, or `None` for Latin (the
/// ranges and the fullwidth/Latin windows below) and for unclaimed letters.
fn script_of(cp: u32) -> Option<&'static str> {
    if cp < 0x0250
        || (0x1E00..=0x1EFF).contains(&cp)
        || (0xFF21..=0xFF3A).contains(&cp)
        || (0xFF41..=0xFF5A).contains(&cp)
    {
        return None;
    }
    for (name, ranges) in SCRIPT_RANGES {
        for &(lo, hi) in *ranges {
            if (lo..=hi).contains(&cp) {
                return Some(name);
            }
        }
    }
    None
}

/// Is `cp` in one of the Latin windows of `_script_counts`?
fn is_latin_cp(cp: u32) -> bool {
    cp < 0x02B0
        || (0x1E00..=0x1EFF).contains(&cp)
        || (0xFF21..=0xFF3A).contains(&cp)
        || (0xFF41..=0xFF5A).contains(&cp)
}

// ------------------------------------------------------------ text extraction

/// `_iter_text`: collect the string leaves of a state, so detection sees real
/// content. Keys are ignored: they are usually English field names. Any
/// mapping counts, not only `dict`. Depth is capped at 6, `None` contributes
/// nothing and undecodable bytes are dropped (`UnicodeDecodeError` -> empty).
fn iter_text(state: &Value, depth: usize, out: &mut Vec<String>) {
    if depth > 6 {
        return;
    }
    match state {
        Value::Null => {}
        Value::String(s) => out.push(s.clone()),
        Value::Number(_) | Value::Bool(_) => {}
        Value::Object(map) => {
            for (_k, v) in map {
                iter_text(v, depth + 1, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                iter_text(v, depth + 1, out);
            }
        }
    }
}

/// `state_text`: flatten a state into the text used for detection (keys are
/// ignored: they are usually English field names). Leaves are joined with a
/// single space and the whole result is truncated to `max_chars` characters.
pub fn state_text(state: &Value, max_chars: usize) -> String {
    let mut leaves = Vec::new();
    iter_text(state, 0, &mut leaves);
    let mut parts: Vec<String> = Vec::new();
    let mut budget = max_chars;
    for leaf in leaves {
        if budget == 0 {
            break;
        }
        let count = leaf.chars().count();
        if count > budget {
            parts.push(leaf.chars().take(budget).collect());
            break;
        }
        parts.push(leaf);
        // Account for the joining space without materializing the full text
        // first.
        budget = budget.saturating_sub(count + 1);
    }
    let joined = parts.join(" ");
    joined.chars().take(max_chars).collect()
}

// ------------------------------------------------------------ script counting

/// `_script_counts`: count the alphabetic characters of `text` by script, in
/// one pass. Latin is inserted last so `script_from_counts` keeps
/// `detect_script`'s tie-break: a named script wins a tie against Latin,
/// because Python's `max` returns the first of equal values and Latin is the
/// last key. An alphabetic character no range claims counts under `"other"`
/// (68% of Unicode's alphabetic codepoints are outside `SCRIPT_RANGES`, and
/// the safe direction is to keep them non-Latin).
pub(crate) fn script_counts(text: &str) -> Vec<(&'static str, usize)> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut latin = 0usize;
    for ch in text.chars() {
        if !py_is_alpha(ch) {
            continue;
        }
        let cp = ch as u32;
        if is_latin_cp(cp) {
            latin += 1;
            continue;
        }
        match script_of(cp) {
            Some(name) => match counts.iter_mut().find(|(n, _)| *n == name) {
                Some((_, c)) => *c += 1,
                None => counts.push((name, 1)),
            },
            None => match counts.iter_mut().find(|(n, _)| *n == "other") {
                Some((_, c)) => *c += 1,
                None => counts.push(("other", 1)),
            },
        }
    }
    counts.push(("latin", latin));
    counts
}

/// `_script_from_counts`: the dominant script, `"unknown"` when there are no
/// letters. Ties go to the first key in insertion order, matching Python's
/// `max` (Latin is inserted last, so a named script beats Latin on a tie).
pub(crate) fn script_from_counts(counts: &[(&'static str, usize)]) -> &'static str {
    if counts.iter().all(|(_, c)| *c == 0) {
        return "unknown";
    }
    let mut best = ("unknown", 0usize);
    for &(name, count) in counts {
        if count > best.1 {
            best = (name, count);
        }
    }
    best.0
}

/// `_profile_from_counts`: per-script fractions, Latin first when present (the
/// order callers saw upstream). Zero-count scripts are dropped.
pub(crate) fn profile_from_counts(counts: &[(&'static str, usize)]) -> Vec<(String, f64)> {
    let total: usize = counts.iter().map(|(_, c)| c).sum();
    if total == 0 {
        return Vec::new();
    }
    let mut ordered: Vec<(String, f64)> = Vec::new();
    for &(name, count) in counts {
        if count == 0 {
            continue;
        }
        let fraction = count as f64 / total as f64;
        if name == "latin" {
            ordered.insert(0, (name.to_string(), fraction));
        } else {
            ordered.push((name.to_string(), fraction));
        }
    }
    ordered
}

/// `detect_script`: dominant script of `text` — `latin`, `han`,
/// `devanagari`, ... or `unknown` if there are no letters.
pub fn detect_script(text: &str) -> &'static str {
    script_from_counts(&script_counts(text))
}

/// `script_profile`: fraction of alphabetic characters belonging to each
/// detected script (Latin first when present).
pub fn script_profile(text: &str) -> Vec<(String, f64)> {
    profile_from_counts(&script_counts(text))
}

// ------------------------------------------------------------ shared words

/// Words that more than one stopword list claims (`_SHARED_WORDS`): a shared
/// word says "not English" without saying *which* language, so it may not name
/// a winner by itself.
fn shared_words() -> &'static HashSet<&'static str> {
    static CELL: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for (_, words) in STOP_LANGUAGES {
            for w in *words {
                *counts.entry(w).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(w, _)| w)
            .collect()
    })
}

/// English function words no other list holds (`_EN_ONLY_WORDS`). They alone
/// carry the English rescue of `latin_profile`.
fn en_only_words() -> &'static HashSet<&'static str> {
    static CELL: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CELL.get_or_init(|| {
        let (en, rest) = STOP_LANGUAGES.split_first().expect("en is first");
        let mut en_only: HashSet<&str> = en.1.iter().copied().collect();
        for (_, words) in rest {
            for w in *words {
                en_only.remove(w);
            }
        }
        en_only
    })
}

/// `_non_english_segment` and `_leaf_non_english` name a language only from
/// the lists in `STOP_LANGUAGES`; this is the same set as a `HashMap` for the
/// two `& _STOP[lang]` lookups. Also read by the Router test that pins the
/// `bn` list as sharing no word with another list.
pub(crate) fn stop_list(lang: &str) -> Option<&'static [&'static str]> {
    STOP_LANGUAGES
        .iter()
        .find(|(l, _)| *l == lang)
        .map(|(_, words)| *words)
}

// ---------------------------------------------------------- token scanners
//
// `_WORD` and `_IDENTIFIER` as linear scanners. `_IDENTIFIER` upstream is the
// regex `(?<![\w-])[\w-]*(?:[.@][\w-]+)+`: dot- and @-joined word runs
// (`github.com`, `user@acme.com`, `v1.2.3`) are identifiers, not prose — they
// split into pieces that collide with real function words (`com` is
// Portuguese for "with"). The lookbehind only keeps the match anchored at the
// start of a word run; the run itself may contain `-`. Because `[\w-]` and
// `[.@]` are disjoint, a leftmost match can only ever begin at a run start,
// so scanning run starts reproduces the regex exactly — and is linear, which
// `test_identifier_complexity.py` pins (the unanchored greedy retry was
// quadratic: 50 000 characters of one token cost 30 s).

/// `py_is_word` for the scanner: `py_is_alnum` minus digits minus `_`.
fn py_word_char(ch: char) -> bool {
    ch != '_' && !ch.is_ascii_digit() && py_is_alnum(ch) && !is_nd(ch)
}

/// `unicodedata` category Nd (the only digits inside Python's `\d`).
fn is_nd(ch: char) -> bool {
    use icu_properties::props::GeneralCategory;
    general_category().get(ch) == GeneralCategory::DecimalNumber
}

/// Maximal runs of `[^\W\d_]+` (`_WORD.findall`).
pub(crate) fn word_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if py_word_char(ch) {
            cur.push(ch);
        } else if !cur.is_empty() {
            runs.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

/// Python `\w`: `str.isalnum()` or the ASCII underscore — the character class
/// of `_IDENTIFIER`'s runs (`[\w-]`, digits and underscore included).
fn is_wc(ch: char) -> bool {
    ch == '_' || py_is_alnum(ch)
}

/// `_IDENTIFIER.sub(" ", text)`: replace every dot-/@-joined identifier run
/// with a space. The regex is `(?<![\w-])[\w-]*(?:[.@][\w-]+)+`; a leftmost
/// match can only start where the lookbehind holds — at a run start, or at a
/// separator whose previous character is outside `[\w-]` (`..com` matches at
/// the second dot) — so scanning those positions reproduces it exactly, and
/// is linear, which `test_identifier_complexity.py` pins (the unanchored
/// greedy retry was quadratic: 50 000 characters of one token cost 30 s).
pub(crate) fn strip_identifiers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let is_run = |ch: char| is_wc(ch) || ch == '-';
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        // the lookbehind (?<![\w-]): a match may only start here when the
        // previous character is outside the run class
        if i > 0 && is_run(chars[i - 1]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while end < chars.len() && is_run(chars[end]) {
            end += 1;
        }
        // (?:[.@][\w-]+)+
        let mut parts = 0usize;
        while end < chars.len()
            && (chars[end] == '.' || chars[end] == '@')
            && end + 1 < chars.len()
            && is_run(chars[end + 1])
        {
            parts += 1;
            end += 1;
            while end < chars.len() && is_run(chars[end]) {
                end += 1;
            }
        }
        if parts > 0 {
            out.push(' ');
            i = end;
        } else if end > start {
            // No match can start inside this run either — every later
            // position fails the lookbehind — so emit the run whole and keep
            // the scan linear (the unanchored retry upstream was quadratic:
            // 50 000 characters of one token cost 30 s).
            out.extend(chars[start..end].iter());
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `_CODE_LINE`: a line carrying code syntax — `=`, `;`, braces, brackets or
/// a call `name(`. The `\w` of `\w\(` is plain Python `\w` (digits included),
/// so the scan uses `is_wc`, not the `_WORD`-flavoured `py_word_char`.
fn is_code_line(line: &str) -> bool {
    if line.contains(['=', ';', '{', '}', '[', ']']) {
        return true;
    }
    let chars: Vec<char> = line.chars().collect();
    for i in 0..chars.len().saturating_sub(1) {
        if chars[i + 1] == '(' && is_wc(chars[i]) {
            return true;
        }
    }
    false
}

/// `_JOINED`: a whitespace token holding a letter/digit, a joiner
/// (`.`, `_`, `/`, `\`) and another letter/digit — an identifier or compound.
/// `[^\W_]` is plain `\w` (every `isalnum` digit, ASCII or not), so `is_wc`.
fn has_joined_token(token: &str) -> bool {
    let chars: Vec<char> = token.chars().collect();
    for w in chars.windows(3) {
        let (a, mid, c) = (w[0], w[1], w[2]);
        if matches!(mid, '.' | '_' | '/' | '\\') && is_wc(a) && is_wc(c) {
            return true;
        }
    }
    false
}

/// Python `str.isupper()` on a whole string: at least one cased character,
/// no lowercase ones — uncased characters (`世`, digits) are ignored, so
/// `"ABC世界"` is upper even though its `世` is not. This is what
/// `_LETTER_RUN.sub` tests per run, not the per-char property.
fn str_is_upper(s: &[char]) -> bool {
    s.iter().any(|c| py_is_upper(*c)) && s.iter().all(|c| !py_is_lower(*c))
}

/// `_LETTER_RUN.sub`: replace runs of 2+ letters (`[^\W\d_]{2,}`) with a
/// space when they are all-caps, keep them otherwise. Upstream only applies
/// it when the prose carries a lowercase letter at all.
fn mask_capitalised_runs(prose: &str) -> String {
    let mut out = String::with_capacity(prose.len());
    let chars: Vec<char> = prose.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if py_word_char(chars[i]) {
            let start = i;
            while i < chars.len() && py_word_char(chars[i]) {
                i += 1;
            }
            let run = &chars[start..i];
            if run.len() >= 2 && str_is_upper(run) {
                out.push(' ');
            } else {
                out.extend(run.iter());
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

// ------------------------------------------------------------ latin profile

/// One `latin_profile` result. `language` is `None` when undecided.
#[derive(Debug, Clone, PartialEq)]
pub struct LatinProfile {
    pub language: Option<String>,
    pub english_hits: usize,
    pub diacritic_rate: f64,
    pub looks_non_english: bool,
}

/// `latin_profile`: evidence behind the Latin-script language guess.
///
/// A non-English language is only named when it matched at least one word no
/// other list claims — shared function words alone (`la`, `e`, `o`) identify
/// no particular language — and beats English by a margin. The stopword
/// tables keep the upstream key order because `max` returns the first of
/// equal values: on a tie the earlier language in `en, fr, de, es, pt, it,
/// nl, ro, bn, az` wins.
pub fn latin_profile(text: &str) -> LatinProfile {
    // 'İ'.lower() is 'i' + a combining dot, which matches no word list.
    let words = word_runs(&strip_identifiers(text).replace('İ', "i").to_lowercase());
    let lowered: String = text.to_lowercase();
    let diac = lowered
        .chars()
        .filter(|ch| NON_EN_DIACRITICS.contains(*ch))
        .count();
    let diac_rate = diac as f64 / std::cmp::max(1, lowered.chars().count()) as f64;
    let non_english = diac_rate >= NON_EN_DIACRITIC_RATE;
    if words.len() < 4 {
        return LatinProfile {
            language: None,
            english_hits: 0,
            diacritic_rate: diac_rate,
            looks_non_english: non_english,
        };
    }

    let mut scores: Vec<(&str, usize)> = Vec::new();
    for (lg, sw) in STOP_LANGUAGES {
        let hits = words.iter().filter(|w| sw.contains(&w.as_str())).count();
        scores.push((lg, hits));
    }
    let en = scores.iter().find(|(lg, _)| *lg == "en").map_or(0, |(_, h)| *h);
    let distinct: HashSet<&str> = words.iter().map(|w| w.as_str()).collect();
    // Only a language that matched at least one word no other list claims may
    // be named. Such a language is dropped from the running rather than
    // merely losing the tie, so a lesser score with real evidence still gets
    // named, and the text stays undecided when no list has any. Ties keep the
    // upstream key order (Python's `max` returns the first of equal values).
    let mut evidenced: Vec<(&str, usize)> = Vec::new();
    for (lg, hits) in &scores {
        if *lg == "en" {
            continue;
        }
        let own = stop_list(lg)
            .map(|list| {
                distinct
                    .iter()
                    .any(|w| list.contains(w) && !shared_words().contains(w))
            })
            .unwrap_or(false);
        if own {
            evidenced.push((lg, *hits));
        }
    }
    let best = evidenced.iter().max_by_key(|(_, h)| *h).copied();

    let lang = if let Some((best_lg, best_score)) = best {
        if best_score >= std::cmp::max(2, en + 2) {
            // a non-English language needs a clear margin over English function words
            Some(best_lg.to_string())
        } else if non_english && best_score >= std::cmp::max(2, en) {
            // Needs two hits here too. One shared function word ("para" in
            // Turkish text) named Spanish on the strength of the diacritics
            // alone, which is a guess dressed as a detection.
            Some(best_lg.to_string())
        } else if en > 0 && (!non_english || english_rescued_by_words(&words, diac_rate)) {
            Some("en".to_string())
        } else {
            None
        }
    } else if en > 0 && (!non_english || english_rescued_by_words(&words, diac_rate)) {
        Some("en".to_string())
    } else {
        None
    };
    LatinProfile {
        language: lang,
        english_hits: en,
        diacritic_rate: diac_rate,
        looks_non_english: non_english,
    }
}

/// `_english_rescued_by_words`: whether plain-English function words outvote
/// a marginal diacritic rate (#337). Two distinct function words no other
/// list holds, at most one word carrying a non-English letter, and a rate
/// still well below `ENGLISH_RESCUE_DIACRITIC_RATE`.
fn english_rescued_by_words(words: &[String], diac_rate: f64) -> bool {
    if diac_rate >= ENGLISH_RESCUE_DIACRITIC_RATE {
        return false;
    }
    let distinct: HashSet<&str> = words.iter().map(|w| w.as_str()).collect();
    if distinct.iter().filter(|w| en_only_words().contains(**w)).count() < 2 {
        return false;
    }
    distinct
        .iter()
        .filter(|w| w.chars().any(|ch| NON_EN_DIACRITICS.contains(ch)))
        .count()
        <= 1
}

/// `guess_latin_language`: best-effort language code for Latin-script text,
/// or `None` when undecided. Short inputs usually return `None` on purpose.
pub fn guess_latin_language(text: &str) -> Option<String> {
    latin_profile(text).language
}

// ------------------------------------------------------- non-Latin words

/// `_non_latin_words`: non-Latin runs that read as words rather than as
/// annotation inside English prose. A symbol (`Set α to 0.05`, one letter), a
/// capitalised proper name and a pronunciation (`[vlɐˈdʲimʲɪr]`, which no
/// script range claims) are excluded; a combining mark belongs to the letter
/// before it and never splits a word.
fn non_latin_words(text: &str) -> Vec<String> {
    let mut runs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut script: Option<&'static str> = None;
    for ch in text.chars() {
        if py_is_combining(ch) {
            continue;
        }
        let s = script_of_char(ch);
        if s.is_some() && s == script {
            cur.push(ch);
            continue;
        }
        if !cur.is_empty() {
            runs.push(std::mem::take(&mut cur));
        }
        match s {
            Some(s) => {
                cur.push(ch);
                script = Some(s);
            }
            None => {
                cur.clear();
                script = None;
            }
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs.into_iter()
        .filter(|w| w.chars().count() >= 2 && !py_is_upper(w.chars().next().unwrap()))
        .collect()
}

/// `_script_of` on a character.
fn script_of_char(ch: char) -> Option<&'static str> {
    script_of(ch as u32)
}

// ------------------------------------------------------------- analysis

/// One `analyse` result.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub script: String,
    pub script_profile: Vec<(String, f64)>,
    pub language: Option<String>,
    pub is_english: bool,
    pub language_undecided: bool,
    pub diacritic_rate: f64,
    pub non_latin_fraction: f64,
    pub mixed_segment: Option<String>,
}

/// `_analyse_text`: detection result for one already-flattened string.
fn analyse_text(text: &str) -> Analysis {
    let counts = script_counts(text);
    let prof = profile_from_counts(&counts);
    let mut script = script_from_counts(&counts).to_string();
    let prof_latin = prof.iter().find(|(s, _)| s == "latin").map_or(0.0, |(_, v)| *v);
    let non_latin = if prof.is_empty() { 0.0 } else { round4(1.0 - prof_latin) };
    let n_alpha = text.chars().filter(|ch| py_is_alpha(*ch)).count();
    let n_non_latin = py_round(non_latin * n_alpha as f64);
    if script == "latin"
        && !non_latin_words(text).is_empty()
        && (non_latin >= NON_LATIN_FRACTION
            || (non_latin >= NON_LATIN_MIN_FRACTION
                && n_non_latin >= NON_LATIN_MIN_LETTERS as f64))
    {
        // the strongest non-Latin script in the profile decides; Python's
        // `max(..., key=prof.get)` returns the first of equal values, and the
        // profile keeps Latin first with the rest in insertion order
        let mut best: Option<&(String, f64)> = None;
        for entry in prof.iter().filter(|(s, _)| s != "latin") {
            if best.is_none_or(|b| entry.1 > b.1) {
                best = Some(entry);
            }
        }
        if let Some((s, _)) = best {
            script = s.clone();
        }
    }
    if script == "unknown" {
        return Analysis {
            script: "unknown".to_string(),
            script_profile: prof,
            language: None,
            is_english: true,
            language_undecided: true,
            diacritic_rate: 0.0,
            non_latin_fraction: 0.0,
            mixed_segment: None,
        };
    }
    if script != "latin" {
        return Analysis {
            script,
            script_profile: prof,
            language: None,
            is_english: false,
            language_undecided: true,
            diacritic_rate: 0.0,
            non_latin_fraction: non_latin,
            mixed_segment: None,
        };
    }
    let prof_lat = latin_profile(text);
    let lang = prof_lat.language.clone();
    // Undecided is not English. Treating it as English sent every Latin-script
    // language we hold no stopwords for to the checkpoint that cannot read it,
    // silently.
    let undecided = lang.is_none();
    let english = lang.as_deref() == Some("en")
        || (undecided && !prof_lat.looks_non_english);
    Analysis {
        script: "latin".to_string(),
        script_profile: prof,
        language: lang,
        is_english: english,
        language_undecided: undecided,
        diacritic_rate: round4(prof_lat.diacritic_rate),
        non_latin_fraction: non_latin,
        mixed_segment: None,
    }
}

/// `_named_prose_language`: language code for one non-code line, or `None`
/// when it does not name a foreign language. Same evidence bar as
/// `_non_english_segment`: four words, a language `latin_profile` will name,
/// and two *different* words of that language. Acronyms and slash compounds
/// are not words.
fn named_prose_language(segment: &str) -> Option<String> {
    if segment.trim().is_empty() || is_code_line(segment) {
        return None;
    }
    let mut kept: Vec<&str> = segment.split_whitespace().collect();
    kept.retain(|tok| !has_joined_token(tok));
    let prose = kept.join(" ");
    let prose = if prose.chars().any(py_is_lower) {
        mask_capitalised_runs(&prose)
    } else {
        prose
    };
    let tokens = word_runs(&prose);
    if tokens.len() < 4 {
        return None;
    }
    let lang = latin_profile(&prose).language?;
    if lang == "en" {
        return None;
    }
    let list = stop_list(&lang)?;
    let distinct: HashSet<String> = tokens.iter().map(|w| w.to_lowercase()).collect();
    if distinct.iter().filter(|w| list.contains(&w.as_str())).count() < 2 {
        return None;
    }
    Some(lang)
}

/// `_non_english_segment`: first line or field that, read on its own, is
/// named a non-English language. Returns `(language, segment)`. A segment
/// needs the evidence a whole state needs — at least four words, and a
/// language named by `latin_profile` — and two *different* words of it; reads
/// at most `max_chars` characters in all.
fn non_english_segment(state: &Value, max_chars: usize) -> Option<(String, String)> {
    let mut seen = 0usize;
    let mut leaves = Vec::new();
    iter_text(state, 0, &mut leaves);
    for leaf in leaves {
        for seg in leaf.split('\n') {
            if seen >= max_chars {
                return None;
            }
            let seg: String = seg.chars().take(max_chars - seen).collect();
            seen += seg.chars().count();
            if let Some(lang) = named_prose_language(&seg) {
                return Some((lang, seg.trim().to_string()));
            }
        }
    }
    None
}

/// `_leaf_non_english`: a string value that is itself not safe for the
/// English checkpoint, else `None`. Each line is capped at 4000 characters;
/// unlike the segment scan, a long earlier field does not consume the budget
/// of the next one (#384).
fn leaf_non_english(leaf: &str) -> Option<Analysis> {
    let mut best_n: i64 = -1;
    let mut best: Option<Analysis> = None;
    for line in leaf.split('\n') {
        let sample: String = line.chars().take(4000).collect();
        if sample.trim().is_empty() || is_code_line(&sample) {
            continue;
        }
        let det = analyse_text(&sample);
        if det.is_english {
            continue;
        }
        if det.language.is_some() && det.language.as_deref() != Some("en") {
            if named_prose_language(&sample).is_none() {
                continue;
            }
        } else if det.script != "latin" && det.script != "unknown" {
            let alpha = sample.chars().filter(|ch| py_is_alpha(*ch)).count();
            if non_latin_words(&sample).is_empty() || alpha < NON_LATIN_MIN_LETTERS {
                continue;
            }
        } else {
            let words = word_runs(&sample);
            if !(det.language_undecided
                && det.diacritic_rate >= NON_EN_DIACRITIC_RATE
                && words.len() >= 4)
            {
                continue;
            }
        }
        let n_alpha = sample.chars().filter(|ch| py_is_alpha(*ch)).count() as i64;
        if n_alpha > best_n {
            best_n = n_alpha;
            best = Some(det);
        }
    }
    best
}

/// `analyse`: full detection result for a state — `script`, `script_profile`,
/// `language` (best effort, may be `None`), `is_english`,
/// `non_latin_fraction` and `mixed_segment` (the line or field that made a
/// mostly English state non-English, else `None`).
///
/// String values are what get read. When a state has several of them, one
/// non-English value is enough: joining every value into one window let a
/// long English note fill the 4000 characters, or outvote a short German
/// message, and that message was then sent to the English checkpoint (#384).
/// The segment scan still stops at 4000 characters, which is what keeps a
/// huge field cheap; a value it did not reach is read on its own afterwards.
pub fn analyse(state: &Value) -> Analysis {
    let mut result = analyse_text(&state_text(state, 4000));
    if result.script == "latin" && result.is_english {
        // A Portuguese ticket with an English stack trace, error payload or
        // form template reads as English as a whole, because the English part
        // is longer — yet the part a question is about is the customer's, and
        // the English checkpoint cannot read it. The cost is lopsided:
        // English sent to multilingual loses a few points, the reverse loses
        // calibration. So a state that would go to English is checked line by
        // line and field by field.
        let leaves = {
            let mut leaves = Vec::new();
            iter_text(state, 0, &mut leaves);
            leaves
        };
        // a single line has no other part to be outvoted by, and was just read whole
        if (leaves.len() > 1 || leaves.iter().any(|leaf| leaf.contains('\n')))
            && let Some((lang, mixed)) = non_english_segment(state, 4000)
        {
            result.language = Some(lang);
            result.is_english = false;
            result.language_undecided = false;
            result.mixed_segment = Some(mixed);
        }
    }
    // A plain string was just read whole. A structured state can still hide a
    // message past the segment cap, or in a script `latin_profile` does not
    // name.
    if state.is_string() || !result.is_english {
        return result;
    }
    let mut best_n: i64 = -1;
    let mut best: Option<Analysis> = None;
    let mut leaves = Vec::new();
    iter_text(state, 0, &mut leaves);
    for leaf in &leaves {
        if let Some(det) = leaf_non_english(leaf) {
            let n_alpha = leaf
                .chars()
                .take(4000)
                .filter(|ch| py_is_alpha(*ch))
                .count() as i64;
            if n_alpha > best_n {
                best_n = n_alpha;
                best = Some(det);
            }
        }
    }
    let Some(best) = best else {
        return result;
    };
    result.language = best.language;
    result.is_english = false;
    result.language_undecided = best.language_undecided;
    result
}

/// `is_english`: `true` when the English checkpoint can be expected to read
/// this state.
pub fn is_english(state: &Value) -> bool {
    analyse(state).is_english
}

#[cfg(test)]
mod tests;

/// The whole stopword table (`_STOP`), for the Router test that pins the
/// `bn` list as sharing no word with another list.
#[cfg(test)]
pub(crate) fn stop_lists() -> &'static [(&'static str, &'static [&'static str])] {
    STOP_LANGUAGES
}
