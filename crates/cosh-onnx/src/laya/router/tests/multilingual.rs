//! Multilingual routing: mixed states and the per-language text families
//! (plain-ASCII Romance #172, Brazilian support text, romanized Bangla,
//! plain-ASCII German #54, accented loanwords in English #337).

use super::*;

// ------------------------------------------------------------- mixed states
// Any line or field that on its own is named a non-English language wins over
// the English text around it. The detector halves (is_english, the reported
// segment) are asserted in `lang/tests.rs`; these are the routing halves.
const TRACE: &str = "O sistema caiu de novo hoje de manhã, segue o log:\n\
                     Traceback (most recent call last):\n\
                     \x20 File \"/app/main.py\", line 42, in handler\n\
                     \x20   return self.process(request)\n\
                     ConnectionError: the connection to the database was refused because the \
                     pool is exhausted and there is no available slot for this request";

#[test]
fn mixed_states_route_multilingual() {
    let r = Router::new().expect("router");
    for (label, state) in [
        ("portuguese ticket + english traceback", json!(TRACE)),
        (
            "portuguese field + english error payload",
            json!({"descricao": "O pagamento não foi processado",
                   "error": {"code": "card_declined", "message": "Your card was declined. Please try again with a different card or contact your bank for more information."}}),
        ),
        (
            "english form template + portuguese body",
            json!({"subject": "New ticket from the web form", "body": "Quero cancelar meu plano"}),
        ),
        (
            "parenthesis in prose is not code",
            json!({"subject": "Urgent: production is down for all customers since the last deploy and the status page is red for the whole region",
                   "body": "Deu erro (500) no login, alguém pode ver isso agora?"}),
        ),
        (
            "english ticket + portuguese error log",
            json!(
                "Our Brazilian branch cannot issue invoices since this morning. The system shows this message:\nERRO: Não foi possível emitir a nota fiscal, o certificado digital está vencido\nCan you help us before the end of the day?"
            ),
        ),
        (
            "english ticket + german error log",
            json!(
                "The nightly sync to the Munich server keeps failing and we lose the whole batch.\nFehler: Die Verbindung zum Server wurde unterbrochen, bitte versuchen Sie es spaeter noch einmal\nPlease check the firewall rules on your side."
            ),
        ),
        (
            "english ticket + spanish error payload",
            json!({"subject": "Payment failed for a customer in Madrid",
                   "description": "The customer tried three times with the same card and each attempt was declined by the gateway, so we would like to know whether the problem is on our side or with the bank.",
                   "error": {"code": "card_declined",
                             "message": "La tarjeta fue rechazada por el banco emisor, contacte con su banco"}}),
        ),
        // acronyms are dropped only from mixed-case text: a line written all
        // in capitals keeps its words
        (
            "all-caps portuguese line",
            json!(
                "This is the fourth email I have sent about the same order and nobody has answered any of them.\nThe customer wrote this in the chat and then closed the window:\nQUERO MEU DINHEIRO DE VOLTA AGORA\nCould someone from the billing team look at order 5512 today?"
            ),
        ),
    ] {
        assert_eq!(
            r.route(&state, Some(&empty_questions()), &RouteOptions::default())
                .expect("route")
                .model,
            "multilingual",
            "mixed/routes multilingual: {label}"
        );
    }
    assert!(
        r.route(
            &json!(TRACE),
            Some(&empty_questions()),
            &RouteOptions::default()
        )
        .expect("route")
        .reason
        .contains("a line or field reads as 'pt'"),
        "mixed/reason names the segment"
    );
}

// ------------------------------------------- plain-ASCII Romance text (#172)
// A state that lost its accents carries no diacritic rate for the
// non-English signal to read, so the function-word lists are the only
// evidence left. The `guess_latin_language` / `is_english` halves are ported
// in `lang/tests.rs`; here are the routing halves.
#[test]
fn plain_ascii_romance_routes_multilingual() {
    let r = Router::new().expect("router");
    for (lang, text) in [
        (
            "es",
            "El pedido llego roto y nadie responde cuando escribo al soporte",
        ),
        ("es", "Quiero cancelar mi plan y pedir un reembolso"),
        ("es", "La factura tiene un error en el importe total"),
        (
            "es",
            "Necesito que me devuelvan el dinero de la compra duplicada",
        ),
        (
            "it",
            "Il cliente e stato addebitato due volte e vuole un rimborso",
        ),
        (
            "it",
            "Voglio cancellare il mio abbonamento e chiedere un rimborso",
        ),
        ("it", "La fattura contiene un errore nell importo totale"),
        (
            "pt",
            "O cliente foi cobrado duas vezes e quer o dinheiro de volta",
        ),
        (
            "fr",
            "Le client a ete facture deux fois et demande un remboursement",
        ),
        (
            "fr",
            "Je ne peux pas acceder a mon compte et j ai besoin d aide",
        ),
    ] {
        assert_eq!(
            guess_latin_language(text).as_deref(),
            Some(lang),
            "latin_lang/plain ascii"
        );
        assert!(
            !is_english(&json!(text)),
            "is_english/plain ascii {lang} {text:?}"
        );
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "multilingual",
            "route/plain ascii {lang} {text:?}"
        );
    }
    // the accented spellings must keep working: those route on the diacritic
    // rate
    for (lang, text) in [
        (
            "es",
            "La facturación tiene un error y necesito una corrección urgente",
        ),
        (
            "it",
            "La fattura è sbagliata, devo avere un rimborso per il pagamento",
        ),
        (
            "fr",
            "La commande est arrivée cassée et personne ne répond au support",
        ),
    ] {
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "multilingual",
            "route/accented {lang}"
        );
    }
    // English must not move for this: `de facto`, `et al.`, `e.g.`,
    // `la carte`, `UN`, `MI5`, `DOS` are ordinary English tokens as well.
    for text in [
        "The customer was charged twice and wants a refund for this invoice",
        "Please cancel my subscription and refund the duplicate charge today",
        "The report by Smith et al. shows the de facto standard, e.g. the LA office and Rio",
        "Our MI5 and UN contacts discussed the DOS attack in LA last month",
        "No refund was issued, so I am writing to you again about invoice 4411",
        "no refund no reply",
        "The son of the director filed a complaint about the duplicate invoice",
    ] {
        assert!(
            is_english(&json!(text)),
            "is_english/romance control {text:?}"
        );
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "english",
            "route/romance control {text:?}"
        );
    }
    // A word several lists claim (`la`, `e`, `o`) says "not English" without
    // saying *which* language, so it may not name one on its own — the same
    // rule as the 0-0 tie above.
    assert_eq!(
        analyse(&json!("Cât e ora acum la Tokyo")).language,
        None,
        "latin_lang/shared words alone name nothing"
    );
    assert_eq!(
        r.route(
            &json!("Cât e ora acum la Tokyo"),
            Some(&empty_questions()),
            &RouteOptions::default()
        )
        .expect("route")
        .model,
        "multilingual",
        "route/shared words still multilingual"
    );
    // ...but a distinctive word in the same state is enough to name the
    // language it belongs to.
    assert_eq!(
        guess_latin_language("La fattura contiene un errore nell importo totale").as_deref(),
        Some("it"),
        "latin_lang/distinctive word names the language"
    );
    assert_eq!(
        guess_latin_language("La factura tiene un error en el importe total").as_deref(),
        Some("es"),
        "latin_lang/shared hits still count toward a named language"
    );
}

// ------------------------------------------------ Brazilian support text
// Short Brazilian messages lean on `você`/`vc`, the unaccented `nao`/`voce`
// and `gostaria`, none of which the unaccented `pt` list held, so each
// matched one word and went to the English checkpoint — which on `pt`
// reports 0.97 mean confidence at 0.47 accuracy (ECE 0.51).
#[test]
fn brazilian_support_text_routes_multilingual() {
    let r = Router::new().expect("router");
    for text in [
        "Boa tarde, gostaria de cancelar o plano",
        "Voce pode me mandar a nota fiscal?",
        "Você pode me mandar a nota fiscal?",
        "Nao consigo fazer login no app",
        "Pix nao caiu na conta",
        "Gostaria de saber o prazo de entrega",
        "Estou esperando faz uma semana",
        "Vc pode cancelar pra mim?",
        // a bug report whose jargon is English keeps only these words to say
        // it is Portuguese
        "Deu erro 500 no endpoint de login depois do update",
        "Depois da atualizacao ninguem consegue logar",
        "Antes funcionava, agora deu pau",
        "Estava tudo certo ate a migracao",
        "Entao o sistema travou de novo",
    ] {
        assert_eq!(
            guess_latin_language(text).as_deref(),
            Some("pt"),
            "latin_lang/pt-br"
        );
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "multilingual",
            "route/pt-br {text:?}"
        );
    }
    // each added word that is also an English token must not move English
    // text
    for text in [
        "Our Sao Paulo office still has not received the invoice",
        "My VC asked for the cap table and the invoice",
        "Nossa Cafe charged my card twice this month",
        "The Boa Vista branch reported an outage this morning",
        "The pra team will review the claim tomorrow",
    ] {
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "english",
            "route/pt-br control {text:?}"
        );
    }
}

// -------------------------------------------------------- romanized Bangla
// Bangla is often typed in Latin letters ("Banglish") when no Bengali
// keyboard is at hand. It has no diacritics and matched no stopword list, so
// it was reported `is_english=True` and handed to the English checkpoint,
// which scores 0.08 on Bangla MASSIVE at 0.94 confidence.
#[test]
fn romanized_bangla_routes_multilingual() {
    let r = Router::new().expect("router");
    for text in [
        "amar kach theke duibar taka kata hoyeche, doya kore ferot din",
        "ami invoice er jonno duibar charge peyechi, refund chai",
        "Ami ei product ta niye khub hotash, ekhon e cancel korte chai",
        "apnara keno amar call dhorchen na? ajke kichu ekta korun",
        "bhai amar account e login korte parchi na",
        "taka ekhono ferot paini, kobe pabo?",
        "order ta kobe asbe bolte parben?",
    ] {
        assert_eq!(
            guess_latin_language(text).as_deref(),
            Some("bn"),
            "latin_lang/banglish"
        );
        assert!(!is_english(&json!(text)), "is_english/banglish {text:?}");
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "multilingual",
            "route/banglish {text:?}"
        );
    }
    assert_eq!(
        r.route(
            &json!("আমার কাছ থেকে দুইবার টাকা কাটা হয়েছে"),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route")
        .model,
        "multilingual",
        "route/bengali script"
    );
    // English must not move. `chai`, `ar`, `ami`, `koto`, `kore`, `oi` are
    // Bangla words that also turn up in English text as a drink, an acronym
    // or a name; one of them next to English function words stays English.
    for text in [
        "Chai latte order was charged twice, please refund the extra amount",
        "The AR team says the ETA for the fix is Friday",
        "Ami Patel from the Koto office sent the invoice to Kore Ltd",
        "Our AR and VR demo in Oi Bahia went well, the client wants a quote",
        "Take the age of the account into account before you refund",
    ] {
        assert!(
            is_english(&json!(text)),
            "is_english/banglish control {text:?}"
        );
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "english",
            "route/banglish control {text:?}"
        );
    }
    // ...and no other language may move either: the `bn` list claims no word
    // another list holds, and leaves out Romance words such as `ora`, `nei`,
    // `vai`.
    let bn = crate::lang::stop_list("bn").expect("bn list");
    let shared: Vec<&str> = bn
        .iter()
        .filter(|w| {
            crate::lang::stop_lists()
                .iter()
                .any(|(lg, words)| *lg != "bn" && words.contains(w))
        })
        .copied()
        .collect();
    assert!(
        shared.is_empty(),
        "latin_lang/bn list shares no word with another list: {shared:?}"
    );
    assert_eq!(
        guess_latin_language("Ei nu sunt de acord cu factura, vreau o corecție").as_deref(),
        Some("ro"),
        "latin_lang/romanian with ei stays romanian"
    );
}

// ------------------------------------------- plain-ASCII German text (#54)
// No umlaut for the diacritic rate to catch, and `in`/`was` counted for
// English alone, so these were labelled English and handed to the checkpoint
// that cannot read them.
#[test]
fn plain_ascii_german_routes_multilingual() {
    let r = Router::new().expect("router");
    for text in [
        "trage diesen termin in meinen kalender ein",
        "wie lautet die temperatur in fulda in hessen",
        "schalte das licht im wohnzimmer aus",
        "was ist die aktuelle zeit",
    ] {
        assert_eq!(
            guess_latin_language(text).as_deref(),
            Some("de"),
            "latin_lang/ascii german"
        );
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "multilingual",
            "route/ascii german {text:?}"
        );
    }
    assert_eq!(
        r.route(
            &json!("I would like to book a flight to Berlin tomorrow"),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route")
        .model,
        "english",
        "route/english control for #54"
    );
    // English that shares words with the German list stays English. `in` and
    // `den` leave the first, a real en-US MASSIVE utterance, one German hit
    // short of flipping; the chat line carries `im` and flips if any one of
    // the English words `am`, `an` or `so` joins the German list.
    for text in [
        "turn off smart lamp in den",
        "im so sorry, am an hour late, stuck in traffic",
    ] {
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "english",
            "route/english sharing german words {text:?}"
        );
    }
    // German words that Spanish (`es`) or French (`du`) also claim would stop
    // naming those languages
    assert_eq!(
        guess_latin_language("que hora es en australia").as_deref(),
        Some("es"),
        "latin_lang/spanish es stays evidence"
    );
    assert_eq!(
        guess_latin_language("baisse le volume du haut-parleur").as_deref(),
        Some("fr"),
        "latin_lang/french du stays evidence"
    );
}

// ------------------------------------- accented loanwords in English (#337)
// The diacritic rate is measured over every character, so one `é` in a short
// English sentence clears the 0.02 floor and used to veto the English
// resolution outright. English wins the veto back only through the word
// rescue: at least two distinct function words no other list holds, and at
// most one word carrying a non-English letter.
#[test]
fn accented_loanwords_stay_english() {
    let r = Router::new().expect("router");
    for text in [
        "Please send me the café menu today please",
        "Could you email me your résumé before the meeting",
        "Send the invoice to José before Friday",
        "We visited Zürich last summer and loved it",
    ] {
        assert_eq!(
            guess_latin_language(text).as_deref(),
            Some("en"),
            "latin_lang/loanword english stays english {text:?}"
        );
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "english",
            "route/loanword english stays english {text:?}"
        );
    }
    // Genuinely non-English accented text keeps its multilingual routing: a
    // German sentence with umlauts and no English function word is not
    // rescued.
    assert!(
        !is_english(&json!("Grüße aus Köln, wir melden uns wegen der Rechnung")),
        "latin_lang/accented german stays non-english"
    );
    assert_eq!(
        r.route(
            &json!("Grüße aus Köln, wir melden uns wegen der Rechnung"),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route")
        .model,
        "multilingual",
        "route/accented german stays multilingual"
    );
    // Danish and Swedish hold no list here, and their accented
    // function-word sentences pick up just one or two English-shaped words
    // (`i`, `at`, `for`, `have`), which is not the two-distinct-word English
    // the rescue requires.
    for text in [
        "sluk lyset i soveværelset",
        "kan jeg få en refundering for det dobbelte beløb",
        "stäng av ljuset i sovrummet",
        "jag vill ha en återbetalning för den dubbla avgiften",
    ] {
        assert!(
            !is_english(&json!(text)),
            "latin_lang/nordic accented stays non-english"
        );
        assert_eq!(
            r.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "multilingual",
            "route/nordic accented stays multilingual {text:?}"
        );
    }
    // Two non-English-letter words is a running non-English vocabulary, not
    // one loanword: the rescue does not fire even with English function
    // words present.
    assert!(
        !is_english(&json!("The naïve façade needs a fresh coat of paint")),
        "latin_lang/two diacritic words are not one loanword"
    );
}
