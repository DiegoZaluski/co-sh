//! Cleaning and structuring email inputs.
//!
//! The markers cover English, Portuguese and Spanish mail clients. The Router
//! already sends Portuguese and Spanish states to the multilingual checkpoint,
//! but with English-only markers their cleaning was a no-op: Gmail's
//! `Em ... escreveu:`, Outlook's `-----Mensagem original-----`, the
//! `Atenciosamente` sign-off and the confidentiality footer all reached the
//! model, and the quoted history (often a *different* request) weighed on the
//! answer as much as the new message did.
//!
//! The regexes are ported pattern-for-pattern with `fancy-regex` (already in
//! the workspace lockfile through tiktoken-rs, so no new crate version is
//! introduced; plain `regex` cannot express the lookarounds). `re.I` becomes
//! a case-insensitive builder flag; the big sign-off pattern compiles
//! case-sensitively with scoped `(?i:...)` groups exactly as upstream.
//! Python's adjacent raw-string pattern concatenation becomes `concat!`.
//!
//! `email_questions` is defined here: the questions are the email workflow's
//! data, so they travel with the email domain; [`crate::laya::presets`]
//! re-exports them so the upstream `presets` import path keeps working.

use std::sync::LazyLock;

use fancy_regex::{Regex, RegexBuilder};
use serde_json::{json, Map, Value};

use crate::laya::presets::q;


pub fn email_questions(categories: Option<Map<String, Value>>) -> Questions {
    // `categories or {…}`: an empty map is falsy in Python, so it falls back
    // to the default categories rather than producing a choice with no
    // options.
    let categories = match categories {
        Some(map) if !map.is_empty() => map,
        _ => serde_json::from_value(json!({
            "billing": "invoices, payments, refunds",
            "technical": "bugs, outages, integrations",
            "sales": "pricing, demos, new purchases",
            "security": "phishing, scams, account compromise",
            "hr": "hiring, leave, payroll",
            "other": "none of the above",
        }))
        .expect("preset categories are a literal object"),
    };
    let mut out = Map::new();
    out.insert(
        "category".to_string(),
        q(
            "choice",
            "Which team should handle the email in `body`?",
            Some(Value::Object(categories)),
        ),
    );
    out.insert(
        "is_spam".to_string(),
        q(
            "noul",
            "Is this email unsolicited spam or bulk marketing?",
            None,
        ),
    );
    out.insert(
        "is_phishing".to_string(),
        q(
            "noul",
            "Is this email a phishing or scam attempt to steal money, credentials, or personal data?",
            Some(json!({
                "true": "phishing, scam, or fraud",
                "false": "a legitimate email",
            })),
        ),
    );
    out.insert(
        "urgency".to_string(),
        q(
            "score",
            "How urgent is the request in `body`?",
            Some(json!([
                "no time pressure",
                "needs attention soon",
                "blocking issue or hard deadline",
            ])),
        ),
    );
    out.insert(
        "needs_reply".to_string(),
        q("noul", "Does the sender expect a reply?", None),
    );
    out
}

fn ci(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|e| panic!("static email pattern {pattern:?}: {e}"))
}

fn cs(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("static email pattern {pattern:?}: {e}"))
}

/// Python `re.match`: anchored at the start of the string. Every anchored
/// marker here begins with `^`, so `is_match` is equivalent.
fn m(re: &Regex, line: &str) -> bool {
    re.is_match(line).unwrap_or(false)
}

fn any_match(regexes: &[Regex], line: &str) -> bool {
    regexes.iter().any(|re| m(re, line))
}

static QUOTE_HEADERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        ci(r"^\s*On .{0,300}wrote:\s*$"),
        // "Em resposta ao que você escreveu:" is body text; a client's
        // attribution always carries a date
        ci(r"^\s*Em (?=.*\d).{0,300}escreveu:\s*$"),
        ci(r"^\s*El (?=.*\d).{0,300}escribi[óo]:\s*$"),
        ci(r"^\s*-{2,}\s*(Original|Forwarded) Message\s*-{2,}"),
        ci(r"^\s*-{2,}\s*(Mensagem (original|encaminhada)|Mensaje (original|reenviado))\s*-{2,}"),
        ci(r"^\s*_{8,}\s*$"),
        // `From:` opens ordinary prose too ("From: my side the integration
        // works, but please refund..."), and a reply header always carries the
        // sender, so the header is only recognised when an address follows --
        // the same rule as `De:` below. A bare `From: Name` header is caught
        // by HEADER_FROM_NAME/HEADER_NEXT instead, which need the header's
        // own `Sent:`/`Date:` line to tell it apart from a sentence.
        ci(r"^\s*From:\s.*[@<]"),
        // `De:` also opens ordinary Portuguese/Spanish lines
        // ("De: 10/09 a 15/09"), so the Outlook header is only recognised
        // when it carries an address
        ci(r"^\s*De:\s.*[@<]"),
    ]
});

/// Gmail wraps a long attribution line, leaving `fulano@x.com> escreveu:`
/// alone on the next line. That tail cuts too, and takes the `On/Em/El ...`
/// head it belongs to with it.
static ATTRIBUTION_TAIL: LazyLock<Regex> =
    LazyLock::new(|| ci(r"^.{0,120}\S@\S+\s+(wrote|escreveu|escribi[óo]):\s*$"));
static ATTRIBUTION_HEAD: LazyLock<Regex> = LazyLock::new(|| ci(r"^\s*(On|Em|El) (?=.*\d)"));

/// Exchange often leaves the address out of Outlook's reply header
/// ("De: Maria Souza"), so a bare `De:` only cuts when the header's own
/// `Enviado:` line, or a dated `Data:`/`Fecha:` line, follows it. `Para:` is
/// not enough: "De: 10/09 / Para: 15/09" is how a leave request reads.
///
/// The same is true of a bare English `From: Maria Souza`, which is why the
/// marker above needs this rule: the English client lines are the
/// translations of the two `De:` neighbours. A line that only looks like
/// prose still has to be told apart from a header by its neighbours, so the
/// English pair is "From: <name>" followed by "Sent:"/"Date:".
static HEADER_FROM_NAME: LazyLock<Regex> = LazyLock::new(|| ci(r"^\s*(De|From):\s+\S"));
static HEADER_NEXT: LazyLock<Regex> =
    LazyLock::new(|| ci(r"^\s*(Enviad[oa]( em| el)?:\s|Sent:\s|(Data|Fecha|Date):\s.*\d{4})"));

static SIGNATURE_MARKERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        cs(r"^\s*--\s*$"),
        // A closing line is the closing word plus punctuation and at most a
        // name. Anything else on the line is a sentence, and the case of the
        // next word is what separates the two: a name is capitalised, "for"
        // in "Thanks for the quick reply." is not. The closing words are
        // matched case-insensitively, the name is not, so the flag is scoped
        // instead of global. `warmest` and `and/& regards` are closings the
        // alternation did not reach, and a name is capitalised in any script,
        // so the name class excludes the lowercase letters instead of listing
        // the uppercase ones: `Regards, Łukasz` is a sign-off,
        // `Thanks for the reply` is not.
        cs(concat!(
            r"^\s*(?i:best|kind|warmest|warm|many thanks|thanks|thank you|regards|cheers|sincerely)",
            r"(?i:\s+(?:and|&)\s+regards|\s+(?:regards|wishes|again|in advance|a lot|so much|very much))?",
            r"[\s,;:!.]*(?:[^\W\d_a-zß-öø-ÿ][\w'-]*[\s,.]*){0,3}$",
        )),
        ci(r"^\s*sent from my (iphone|android|mobile|ipad)"),
        // Portuguese/Spanish sign-offs match only on their own: "Obrigado pelo
        // retorno, mas ..." is a request, not a signature, so unlike the
        // English marker no trailing words are allowed
        ci(concat!(
            r"^\s*(atenciosamente|att|abraços?|abs|um abraço|cordialmente|grat[oa]|(muito )?obrigad[oa]s?",
            r"( desde já| pela atenção)?|(com os melhores )?cumprimentos|saudações|",
            r"(un )?saludos?( cordiales)?|atentamente|(muchas )?gracias( de antemano)?)[\s,!.]*$",
        )),
    ]
});

/// Mobile and mail-app footers. Only a line that is nothing *but* the footer
/// matches -- "Enviado do meu celular o comprovante ontem." is a request --
/// and such a line may run to 60 characters, since Samsung's default
/// ("Enviado do meu smartphone Samsung Galaxy.") is longer than a sign-off's
/// 40.
const DEVICE: &str = r"iphone|ipad|android|ios|celular|telemóvel|móvil|galaxy|smartphone|samsung|tablet|outlook|yahoo|mail|e-?mail|gmail|windows";
static DEVICE_FOOTER: LazyLock<Regex> = LazyLock::new(|| {
    ci(&format!(
        concat!(
            r"^\s*((enviad[oa] (do|pelo|pela|via|desde|a partir do)( meu| minha| mi)?|sent from( my)?)",
            r#" ({})( ({}|para|for|no|na|\d+))*|(obter o|get) outlook (para|for) (ios|android))[\s.!]*$"#
        ),
        DEVICE,
        DEVICE,
    ))
});

static DISCLAIMER: LazyLock<Regex> = LazyLock::new(|| {
    ci(concat!(
        // English: tied to a disclaimer noun and a disclaimer tail, the way
        // the Portuguese branches below are. The bare word matched any
        // sentence that merely mentioned it, so "Is this confidential?" and
        // "Confidential: I need a refund." were deleted whole. `[^.]` rather
        // than `[^.\n]`: a footer wraps, so "are\nconfidential" must still
        // match.
        r"(\b(e-?mail|message|information|communication|transmission|contents?)\b[^.]{0,60}",
        r"\bconfidential\b[^.]{0,60}\b(intended|solely|addressee|recipient|privileged|",
        r"disclos|unauthori[sz]ed)|",
        r"\bconfidential\b[^.]{0,60}\b(and (may|is) (also )?privileged)|",
        r"if you (have )?received this (e-?mail|message) in error|",
        // Portuguese/Spanish: tied to "this message/e-mail" rather than the
        // bare word `confidencial`, which a sender's own request ("preciso do
        // contrato confidencial") uses just as often
        r"\b(esta|este) (mensagem|e-?mail|mensaje|correo)\b[^.]{0,80}(confidencia|sigilos|privilegiad)|",
        r"\b(uso exclusivo|exclusivamente|únicamente|unicamente)\b[^.]{0,30}",
        r"(destinatári|destinatari|pessoa|persona|entidade|entidad)|",
        r"\b(recebeu|recebido|receber) (esta|este) (mensagem|e-?mail)\b[^.]{0,20} por (engano|erro)|",
        r"\b(ha recibido|recibió|recibe) (este|esta) (mensaje|correo)\b[^.]{0,20} por error|",
        // the "think before printing" footer, tied to its environmental ending
        // rather than to `antes de imprimir`, which a request uses too
        // ("antes de imprimir o boleto, confira o valor")
        r"\bantes de imprimir\b[^.]{0,100}(meio ambiente|medio ambiente|natureza|planeta|realmente necess)|",
        r"\b(meio|medio) ambiente\b[^.]{0,30}antes de imprimir)",
    ))
});

static SENTENCE: LazyLock<Regex> = LazyLock::new(|| cs(r"(?<=[.!?])\s+"));

/// `re.split` over a pattern: split at every non-overlapping match span,
/// separators dropped (Python's `re.split` without a capture group).
fn regex_split(re: &Regex, text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut last = 0usize;
    for span in re.find_iter(text).flatten() {
        parts.push(text[last..span.start()].to_string());
        last = span.end();
    }
    parts.push(text[last..].to_string());
    parts
}

/// `_starts_new_sentence`: first letter is uppercase — a fresh sentence, not
/// a wrapped line.
///
/// Lines in uncased scripts (CJK, Devanagari, ...) never start a new piece,
/// so wrapped boilerplate in those scripts still drops whole.
fn starts_new_sentence(line: &str) -> bool {
    for ch in line.chars() {
        if crate::lang::py_is_alpha(ch) {
            return crate::lang::py_is_upper(ch);
        }
    }
    false
}

/// `_split_fused_lines`: split a fused boilerplate-positive sentence at
/// sentence-starting newlines.
///
/// An unpunctuated request line glued to a disclaimer line ("locked\nThis
/// ...") splits at the newline because the next line starts uppercase; a
/// lowercase continuation ("are\nconfidential") belongs to the same sentence,
/// so a wrapped boilerplate footer still drops whole.
fn split_fused_lines(sentence: &str) -> Vec<String> {
    if !sentence.contains('\n') {
        return vec![sentence.to_string()];
    }
    let mut pieces: Vec<String> = Vec::new();
    let mut buf = String::new();
    for line in sentence.split('\n').map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if !buf.is_empty() && starts_new_sentence(line) {
            pieces.push(std::mem::take(&mut buf));
            buf = line.to_string();
        } else if buf.is_empty() {
            buf = line.to_string();
        } else {
            buf.push(' ');
            buf.push_str(line);
        }
    }
    if !buf.is_empty() {
        pieces.push(buf);
    }
    pieces
}

/// `_strip_disclaimer`: drop boilerplate disclaimer text from one paragraph.
///
/// A paragraph is dropped whole only when *every* sentence in it is
/// boilerplate; otherwise only the boilerplate sentences go. A footer that
/// runs on without a blank line used to take the sender's actual request with
/// it, which is worse than leaving one boilerplate line behind.
fn strip_disclaimer(paragraph: &str) -> String {
    if !m(&DISCLAIMER, paragraph) {
        return paragraph.to_string(); // nothing to do: keep the original line structure
    }
    let parts: Vec<String> = regex_split(&SENTENCE, paragraph)
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let mut pieces: Vec<String> = Vec::new();
    for p in parts {
        if m(&DISCLAIMER, &p) {
            pieces.extend(split_fused_lines(&p));
        } else {
            pieces.push(p);
        }
    }
    pieces
        .iter()
        .filter(|p| !m(&DISCLAIMER, p))
        .cloned()
        .collect::<Vec<String>>()
        .join(" ")
}

/// `re.split(r"\n\s*\n", text)`: split on blank-line runs.
fn split_paragraphs(text: &str) -> Vec<String> {
    static SPLIT: LazyLock<Regex> = LazyLock::new(|| cs(r"\n\s*\n"));
    regex_split(&SPLIT, text)
}

/// `re.sub(r"[ \t]+", " ", ...)`: collapse horizontal whitespace runs.
fn collapse_spaces(text: &str) -> String {
    static WS: LazyLock<Regex> = LazyLock::new(|| cs(r"[ \t]+"));
    WS.replace_all(text, " ").into_owned()
}

/// `clean_email_body`: remove quoted email history, signatures and
/// disclaimers to keep input focused.
pub fn clean_email_body(body: &str, max_chars: usize) -> String {
    // Upstream's `(body or "")` maps the None branch; the Rust surface has no
    // None here, so the string is used as given.
    let text = body
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace("\\n", "\n");
    // Bound regex work before the expensive patterns below: the disclaimer
    // uses [^.]{0,60/80/100} alternations whose cost grows with input
    // length, and only max_chars are ever returned. Truncate lines too so one
    // MB-long line cannot dominate matching.
    let text = if text.chars().count() > max_chars * 4 {
        text.chars().take(max_chars * 4).collect()
    } else {
        text
    };
    let src: Vec<&str> = text.split('\n').collect();
    let mut lines: Vec<String> = Vec::new();
    for (i, line) in src.iter().enumerate() {
        if any_match(&QUOTE_HEADERS, line) && !lines.is_empty() {
            break;
        }
        if !lines.is_empty()
            && m(&HEADER_FROM_NAME, line)
            && i + 1 < src.len()
            && m(&HEADER_NEXT, src[i + 1])
        {
            break;
        }
        if m(&ATTRIBUTION_TAIL, line) && !lines.is_empty() {
            if lines.last().map(|l| m(&ATTRIBUTION_HEAD, l)).unwrap_or(false) {
                lines.pop();
            }
            break;
        }
        if line.trim_start().starts_with('>') {
            continue;
        }
        lines.push(line.trim_end().to_string());
    }
    // The signature scan starts at 60% of the body, never before the first
    // line, and never in the last eight lines.
    // `max(1, min(int(len(lines) * 0.6), len(lines) - 8))`: kept in float so
    // the expression is the literal transcription of the Python one.
    let cut_start = (((lines.len() as f64) * 0.6) as usize)
        .min(lines.len().saturating_sub(8))
        .max(1);
    let mut cut = lines.len();
    for (i, line) in lines.iter().enumerate().skip(cut_start) {
        let n = line.trim().chars().count();
        if (n <= 40 && any_match(&SIGNATURE_MARKERS, line)) || (n <= 60 && m(&DEVICE_FOOTER, line))
        {
            cut = i;
            break;
        }
    }
    let paragraphs: Vec<String> = split_paragraphs(&lines[..cut].join("\n"))
        .into_iter()
        .map(|p| strip_disclaimer(&p))
        .collect();
    let joined = paragraphs
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| collapse_spaces(p.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    // `text[:max_chars]` counts Python characters.
    joined.chars().take(max_chars).collect()
}

/// `email_state`: construct a clean state dictionary for email
/// classification.
///
/// `extra` mirrors the upstream `**extra`: entries whose value is `None` are
/// dropped, everything else is inserted after `from` (insertion order kept by
/// the ordered map).
pub fn email_state(
    subject: &str,
    body: &str,
    sender: Option<&str>,
    clean: bool,
    extra: &[(&str, Value)],
) -> Map<String, Value> {
    let mut state = Map::new();
    state.insert(
        "subject".to_string(),
        Value::String(subject.trim().to_string()),
    );
    state.insert(
        "body".to_string(),
        Value::String(if clean {
            clean_email_body(body, 3000)
        } else {
            body.to_string()
        }),
    );
    if let Some(sender) = sender.filter(|s| !s.is_empty()) {
        state.insert("from".to_string(), Value::String(sender.to_string()));
    }
    for (k, v) in extra {
        if !v.is_null() {
            state.insert((*k).to_string(), v.clone());
        }
    }
    state
}

/// The preset questions dict type, re-exported from the presets module for
/// the `laya.email` import path.
pub type Questions = crate::laya::presets::Questions;

#[cfg(test)]
mod tests;
