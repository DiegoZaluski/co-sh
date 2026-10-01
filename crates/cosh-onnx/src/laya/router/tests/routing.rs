//! Routing decisions: the decision table, english-vs-not, dotted tokens,
//! dict-state routing (#384), unknown-Latin routing (#35) and unlisted
//! scripts.

use super::*;

// ------------------------------------------------- script detection anchors
// The SCRIPTS table is asserted against `detect_script` in `lang/tests.rs`;
// these are the table's Router-facing halves upstream runs next to them.

#[test]
fn the_scripts_table() {
    // Upstream's SCRIPTS table asserts `detect_script` only; the routing
    // consequences of these rows are covered by the is_english/routing
    // tables below, as upstream has them.
    for (label, text, script) in [
        (
            "english",
            "The customer was charged twice and wants a refund.",
            "latin",
        ),
        ("armenian", "Հայերեն", "armenian"),
        ("armenian uppercase", "ՀԱՅԵՐԵՆ", "armenian"),
        ("armenian punctuation only", "։֊", "unknown"),
        ("azerbaijani lone schwa", "ə", "latin"),
        (
            "azerbaijani uppercase",
            "MÜŞTƏRİ İLƏ ƏLAQƏ SAXLAYIN",
            "latin",
        ),
        (
            "french",
            "Le client a été facturé deux fois et demande un remboursement.",
            "latin",
        ),
        (
            "hindi",
            "ग्राहक से दो बार शुल्क लिया गया और वह धनवापसी चाहता है।",
            "devanagari",
        ),
        (
            "japanese",
            "お客様は二重に請求されたため返金を希望しています。",
            "kana",
        ),
        ("chinese", "客户被重复扣款要求退款", "han"),
        ("korean", "고객이 두 번 청구되어 환불을 원합니다", "hangul"),
        (
            "arabic",
            "تم خصم المبلغ مرتين من العميل ويريد استرداد الأموال",
            "arabic",
        ),
        ("tamil", "வாடிக்கையாளரிடம் இருமுறை கட்டணம் வசூலிக்கப்பட்டது", "tamil"),
        (
            "russian",
            "С клиента дважды сняли деньги и он хочет возврат",
            "cyrillic",
        ),
        ("thai", "ลูกค้าถูกเรียกเก็บเงินสองครั้งและต้องการเงินคืน", "thai"),
        (
            "greek",
            "Ο πελάτης χρεώθηκε δύο φορές και θέλει επιστροφή χρημάτων",
            "greek",
        ),
        ("hebrew", "הלקוח חויב פעמיים ורוצה החזר כספי", "hebrew"),
        ("empty", "", "unknown"),
        ("digits only", "12345 6789", "unknown"),
    ] {
        assert_eq!(detect_script(text), script, "script/{label}");
    }
}

// --------------------------------------------------------------- english vs not
// The is_english table itself lives in `lang/tests.rs`; these are its
// routing halves, plus the Azerbaijani overrides upstream pins.

#[test]
fn routing_follows_the_english_detection() {
    for (text, want) in [
        (
            "Please refund the duplicate charge on invoice 4411 today.",
            "english",
        ),
        ("Հայերեն", "multilingual"),
        (
            "Sifarisim gelmedi ve pulum geri qaytarilmadi, zehmet olmasa yoxlayin",
            "multilingual",
        ),
        (
            "Mən sizin xidmətinizdən razı deyiləm və pulumu geri istəyirəm",
            "multilingual",
        ),
        ("refund me", "english"),
        ("ग्राहक से दो बार शुल्क लिया गया", "multilingual"),
        ("お客様は二重に請求されました", "multilingual"),
        ("С клиента дважды сняли деньги", "multilingual"),
        (
            "Le client a été facturé deux fois et il demande un remboursement pour la \
             facture qui a été payée le mois dernier avec la carte de crédit",
            "multilingual",
        ),
        (
            "Der Kunde wurde zweimal belastet und möchte eine Rückerstattung für die \
             Rechnung die nicht korrekt ist und auch nicht bezahlt wurde",
            "multilingual",
        ),
        // Latin-script languages with no stopword list of their own: reported
        // in #35, where Romanian states were handed to the English checkpoint
        // (0.330 accuracy, 0.658 ECE on `ro`) instead of the multilingual one.
        // An unidentified language must never be assumed English.
        (
            "Gătește-mi o rețetă de sarmale de post pentru mâine.",
            "multilingual",
        ),
        (
            "Am fost taxat de două ori pentru factura din luna martie și vreau banii",
            "multilingual",
        ),
        (
            "Klient został obciążony dwukrotnie i chce zwrot pieniędzy za fakturę",
            "multilingual",
        ),
        (
            "Zákazníkovi byla částka účtována dvakrát a žádá o vrácení peněz",
            "multilingual",
        ),
        (
            "Müşteriden iki kez ücret alındı ve para iadesi istiyor lütfen yardım",
            "multilingual",
        ),
        (
            "Khách hàng đã bị thu phí hai lần và muốn được hoàn tiền ngay",
            "multilingual",
        ),
        // English with the odd loanword must not tip over into the
        // multilingual checkpoint
        (
            "We visited a cafe in Zurich and the naive assumption about the \
             invoice was wrong, so please refund the duplicate charge",
            "english",
        ),
    ] {
        assert_eq!(
            is_english(&json!(text)),
            want == "english",
            "is_english/{text:?}"
        );
        assert_eq!(route_text(text).model, want, "route/{text:?}");
    }
}

#[test]
fn undecided_latin_is_flagged_and_routes_on_the_rules() {
    let undecided = "Müşteriden iki kez ücret alındı ve para iadesi istiyor";
    let analysis = analyse(&json!(undecided));
    assert!(analysis.language_undecided, "latin/undecided is flagged");
    assert_eq!(analysis.language, None, "latin/undecided names no language");
    assert!(
        !analyse(&json!("Please refund the duplicate charge on the invoice")).language_undecided,
        "latin/english is not undecided"
    );
    assert!(
        analyse(&json!("Gătește-mi o rețetă de sarmale")).diacritic_rate > 0.02,
        "latin/diacritic rate reported"
    );
    assert_eq!(
        analyse(&json!("Please refund the duplicate charge today")).diacritic_rate,
        0.0,
        "latin/english has no diacritics"
    );

    // every branch of analyse() reports the same keys, so a caller can read
    // one without guarding
    for text in [
        "Please refund the duplicate charge",
        "ग्राहक से दो बार",
        "Gătește-mi o rețetă de sarmale",
        "12345 ???",
    ] {
        let d = default_route(&json!(text), &empty_questions())
            .detection
            .expect("detection present");
        let object = d.as_object().expect("detection is an object");
        for key in [
            "script",
            "script_profile",
            "language",
            "is_english",
            "language_undecided",
            "diacritic_rate",
            "non_latin_fraction",
            "mixed_segment",
        ] {
            assert!(object.contains_key(key), "keys present: {key} for {text:?}");
        }
    }
}

// ------------------------------------------------------------- dotted tokens
// Versions, decimals and dotted abbreviations are identifiers, not prose:
// masking them must not cost the prose around them its language, and a full
// stop ends a sentence rather than joining an identifier.
#[test]
fn dotted_tokens_are_identifiers_not_prose_at_the_router() {
    assert_eq!(
        guess_latin_language(
            "O cliente nao recebeu o produto, mas quer o dinheiro para a conta, veja example.com",
        )
        .as_deref(),
        Some("pt"),
        "latin_lang/portuguese prose with a link"
    );
    assert_eq!(
        guess_latin_language("O cliente nao recebeu o produto, mas quer o dinheiro para a conta.",)
            .as_deref(),
        Some("pt"),
        "latin_lang/portuguese sentence with a full stop"
    );
    assert_eq!(
        default_route(
            &json!({"body": "Please check example.com and acme.com for the invoice"}),
            &empty_questions()
        )
        .model,
        "english",
        "route/english with a link stays english"
    );
}

// ------------------------------------------------- dict-state routing (#384)
// The same German sentence must route the same way as a string and as a dict
// value. English sibling fields used to hide that value: detection then
// reported language_undecided (or English) and the router fell through to the
// English checkpoint. (The UserDict / MappingProxyType / bytes variants have
// no JSON analogue; see the module docs.)
#[test]
fn dict_state_routing_matches_string_state() {
    let de = "Mein Konto wurde zweimal belastet";
    let r = Router::new().expect("router");
    let de_str = r
        .route(
            &json!(de),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route");
    let de_dict = r
        .route(
            &json!({"message": de}),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route");
    assert_eq!(
        de_dict.model, de_str.model,
        "route/dict german matches string"
    );
    assert_eq!(
        de_dict.model, "multilingual",
        "route/dict german is multilingual"
    );
    assert_eq!(
        de_dict.detection.as_ref().expect("detection")["language"],
        json!("de"),
        "route/dict german names de"
    );
    assert_eq!(
        de_dict.detection.as_ref().expect("detection")["language_undecided"],
        json!(false),
        "route/dict german is not undecided"
    );
    assert_eq!(
        analyse(&json!({"message": de})).language,
        analyse(&json!(de)).language,
        "analyse/dict german matches string"
    );

    // English notes must not outvote the message, and a long note must not
    // push it out of the window.
    let de_ticket = json!({
        "ticket_id": "TCK-88213",
        "channel": "web chat",
        "agent_notes": "Please check the shipping status and refund the customer if the charge was duplicated.",
        "message": de,
    });
    assert_eq!(
        r.route(
            &de_ticket,
            Some(&empty_questions()),
            &RouteOptions::default()
        )
        .expect("route")
        .model,
        "multilingual",
        "route/dict german beside english notes"
    );
    assert_eq!(
        r.route(
            &de_ticket,
            Some(&empty_questions()),
            &RouteOptions::default()
        )
        .expect("route")
        .detection
        .expect("detection")["language_undecided"],
        json!(false),
        "route/dict german beside english notes is not undecided"
    );
    let de_long = json!({
        "agent_notes": "Please check the shipping status and tell the customer about the refund. ".repeat(80),
        "message": de,
    });
    assert_eq!(
        r.route(&de_long, Some(&empty_questions()), &RouteOptions::default())
            .expect("route")
            .model,
        "multilingual",
        "route/dict german after long english note"
    );
    assert_eq!(
        r.route(&de_long, Some(&empty_questions()), &RouteOptions::default())
            .expect("route")
            .detection
            .expect("detection")["language"],
        json!("de"),
        "route/dict german after long english note names de"
    );

    // A name beside an English request is not a second message.
    for (state, label) in [
        (
            json!({"name": "Антон Павлович Чехов",
                   "body": "Please refund the duplicate charge on invoice 4411 today."}),
            "route/dict cyrillic name stays english",
        ),
        (
            json!({"name": "José",
                   "body": "Please refund the duplicate charge on invoice 4411 today."}),
            "route/dict jose stays english",
        ),
        (
            json!({"body": "Please refund the duplicate charge on invoice 4411 today."}),
            "route/dict english body stays english",
        ),
    ] {
        assert_eq!(
            r.route(&state, Some(&empty_questions()), &RouteOptions::default())
                .expect("route")
                .model,
            "english",
            "{label}"
        );
    }
}

// ------------------------------------------------------- routing decisions
// The 30-case table: (label, state, questions, overrides, want).
// `model`/`task`/`lang` map to the `RouteOptions` fields of the same names.
#[test]
fn the_routing_decisions_table() {
    let generic = q_generic();
    let td = q_td();
    type Case<'a> = (
        &'a str,
        Value,
        &'a Map<String, Value>,
        RouteOptions<'a>,
        &'a str,
    );
    let cases: &[Case] = &[
        (
            "english text",
            json!({"body": "I was charged twice, please refund."}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        (
            "armenian text",
            json!({"body": "Հայերեն"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "armenian explicit override",
            json!({"body": "Հայերեն"}),
            &generic,
            opts_model("english"),
            "english",
        ),
        (
            "azerbaijani no diacritics",
            json!({"body": "Sifarisim gelmedi ve pulum geri qaytarilmadi, zehmet olmasa yoxlayin"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "azerbaijani explicit override",
            json!({"body": "Müştəridən iki dəfə pul alınıb"}),
            &generic,
            opts_model("english"),
            "english",
        ),
        (
            "hindi text",
            json!({"body": "मुझसे दो बार शुल्क लिया गया"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "japanese text",
            json!({"body": "二重に請求されました"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "korean text",
            json!({"body": "두 번 청구되었습니다"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "arabic text",
            json!({"body": "تم خصم المبلغ مرتين"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        // Latin brand names are the letter plurality here, but the request itself is CJK
        (
            "chinese with a brand",
            json!({"body": "我的 iPhone 15 Pro Max 订单还没到"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "japanese with brands",
            json!({"body": "Amazonで買ったiPhoneが届かない"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "korean with a brand",
            json!({"body": "Samsung Galaxy 주문이 아직 안 왔어요"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "english with a han name",
            json!({"body": "My name is 王小明 and my order is late"}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        // an English wrapper dilutes the share, but the request is still CJK
        (
            "chinese in a ticket",
            json!({"ticket_id": "TCK-88213", "channel": "web chat",
             "agent_notes": "Customer asked about a delayed order. Please check shipping status.",
             "message": "我的订单已经两个星期了还没有到"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "korean after english turns",
            json!([
            {"role": "agent", "text": "Hello! Thanks for contacting support."},
            {"role": "agent", "text": "Could you share your order number please?"},
            {"role": "user", "text": "주문번호는 5521이고 아직 배송이 안 됐어요"}]),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "english with greek symbols",
            json!({"request": "Compute the mean μ and variance σ of X, then P(|X-μ| > 2σ)."}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        // English prose that names someone in their own script: the name is not the request, and a
        // capitalised run, a lone symbol and a pronunciation are all annotation rather than content.
        (
            "english prose, russian name",
            json!({"body": "Anton Pavlovich Chekhov (Russian: Антон Павлович Чехов) was a playwright."}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        (
            "english prose, name with IPA",
            json!({"body": "Vladimir Nabokov (Russian: Влади́мир Набо́ков [vlɐˈdʲimʲɪr nɐˈbokəf]) wrote Lolita and taught literature at Cornell for more than a decade."}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        (
            "english prose, greek name",
            json!({"body": "Eleftherios Venizelos (Greek: Ελευθέριος Βενιζέλος) served as prime minister."}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        (
            "english prose, hebrew name",
            json!({"body": "Amos Oz (Hebrew: עמוס עוז), born Amos Klausner, was an Israeli writer and professor of literature at Ben-Gurion University of the Negev in Beersheba."}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        (
            "german text",
            json!({"body": "Der Kunde wurde zweimal belastet und moechte eine Rueckerstattung fuer die Rechnung die nicht korrekt ist"}),
            &generic,
            RouteOptions::default(),
            "multilingual",
        ),
        (
            "explicit model",
            json!({"body": "anything"}),
            &generic,
            opts_model("multilingual"),
            "multilingual",
        ),
        (
            "explicit model overrides script",
            json!({"body": "मुझसे दो बार"}),
            &generic,
            opts_model("english"),
            "english",
        ),
        (
            "explicit task",
            json!({"body": "x"}),
            &generic,
            opts_task("typed_decisions"),
            "typed-decisions",
        ),
        (
            "explicit lang en",
            json!({"body": "मुझसे दो बार"}),
            &generic,
            opts_lang("en"),
            "english",
        ),
        (
            "explicit lang de",
            json!({"body": "hello there"}),
            &generic,
            opts_lang("de"),
            "multilingual",
        ),
        (
            "td workflow, auto OFF",
            json!({"body": "I was charged twice"}),
            &td,
            RouteOptions::default(),
            "english",
        ),
        (
            "empty state",
            json!({}),
            &generic,
            RouteOptions::default(),
            "english",
        ),
        (
            "none state",
            Value::Null,
            &generic,
            RouteOptions::default(),
            "english",
        ),
    ];
    for (label, state, questions, opts, want) in cases {
        let got = route(state, questions, opts);
        assert_eq!(got.model, *want, "route/{label}");
    }
}

fn opts_model(model: &'static str) -> RouteOptions<'static> {
    RouteOptions {
        model: Some(model),
        ..RouteOptions::default()
    }
}

fn opts_task(task: &'static str) -> RouteOptions<'static> {
    RouteOptions {
        task: Some(task),
        ..RouteOptions::default()
    }
}

fn opts_lang(lang: &'static str) -> RouteOptions<'static> {
    RouteOptions {
        lang: Some(lang),
        ..RouteOptions::default()
    }
}

#[test]
fn auto_task_detection_is_opt_in() {
    let opts = RouterOptions {
        auto_task_detection: true,
        ..RouterOptions::default()
    };
    let r_auto = Router::configure(opts).expect("router");
    let td = q_td();
    let generic = q_generic();
    assert_eq!(
        r_auto
            .route(
                &json!({"body": "I was charged twice"}),
                Some(&td),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
        "typed-decisions",
        "route/td workflow, auto ON"
    );
    assert_eq!(
        r_auto
            .route(
                &json!({"body": "I was charged twice"}),
                Some(&generic),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
        "english",
        "route/auto ON but generic questions"
    );
    // explicit model still beats auto-detected workflow
    assert_eq!(
        r_auto
            .route(
                &json!({"body": "x"}),
                Some(&td),
                &opts_model("multilingual")
            )
            .expect("route")
            .model,
        "multilingual",
        "route/explicit beats workflow"
    );
    // An auto-detected workflow reports `repo` like every other branch does. It used to hand
    // back the raw (repo, subfolder) spec, which serialises to a JSON list instead of the
    // "repo/subfolder" string.
    assert_eq!(
        r_auto
            .route(
                &json!({"body": "I was charged twice"}),
                Some(&td),
                &RouteOptions::default()
            )
            .expect("route")
            .repo,
        "convaiinnovations/laya/typed-decisions",
        "route/auto workflow repo is a string"
    );
    assert_eq!(
        r_auto
            .route(
                &json!({"body": "I was charged twice"}),
                Some(&td),
                &RouteOptions::default()
            )
            .expect("route")
            .repo,
        r_auto
            .route(
                &json!({"body": "I was charged twice"}),
                Some(&td),
                &opts_task("typed_decisions")
            )
            .expect("route")
            .repo,
        "route/auto workflow repo matches explicit task"
    );
    let alone = Router::configure(RouterOptions {
        auto_task_detection: true,
        standalone_repos: true,
        ..RouterOptions::default()
    })
    .expect("router");
    assert_eq!(
        alone
            .route(&json!({"body": "x"}), Some(&td), &RouteOptions::default())
            .expect("route")
            .repo,
        "convaiinnovations/laya-typed-decisions",
        "route/auto workflow repo standalone"
    );
}

#[test]
fn the_decision_payload_shape() {
    let r = Router::new().expect("router");
    let generic = q_generic();
    let d = r
        .route(
            &json!({"body": "मुझसे दो बार शुल्क लिया गया"}),
            Some(&generic),
            &RouteOptions::default(),
        )
        .expect("route");
    assert_eq!(
        d.repo, "convaiinnovations/laya/multilingual",
        "decision/has repo"
    );
    assert!(!d.reason.is_empty(), "decision/has reason");
    assert_eq!(
        d.detection.as_ref().expect("detection")["script"],
        json!("devanagari"),
        "decision/detection script"
    );
    assert_eq!(d.model, "multilingual", "decision/.model property");
    // `decision/is dict`: the payload serialises as the same JSON object
    // upstream's dict(decision) does, in the same key order.
    let as_value = d.to_value();
    let keys: Vec<&String> = as_value.as_object().expect("object").keys().collect();
    assert_eq!(
        keys,
        ["model", "repo", "reason", "detection", "workflow"]
            .map(String::from)
            .iter()
            .collect::<Vec<_>>(),
        "decision/keys in upstream insertion order"
    );
}

#[test]
fn a_custom_default_override() {
    let r = Router::configure(RouterOptions {
        default: Some("multilingual".to_string()),
        ..RouterOptions::default()
    })
    .expect("router");
    assert_eq!(
        r.route(
            &json!("12345"),
            Some(&empty_questions()),
            &RouteOptions::default()
        )
        .expect("route")
        .model,
        "multilingual",
        "route/custom default"
    );
}

// ------------------------------------------------- unknown-Latin routing (#35)
#[test]
fn unknown_latin_routing() {
    let r = Router::new().expect("router");
    for (label, text) in [
        (
            "romanian",
            "Gătește-mi o rețetă de sarmale de post pentru mâine.",
        ),
        (
            "romanian agent request",
            "Exportă APK-ul pentru Android și pune-l pe Drive ca să-l instalez.",
        ),
        (
            "polish",
            "Klient został obciążony dwukrotnie i chce zwrot pieniędzy za fakturę",
        ),
        (
            "turkish",
            "Müşteriden iki kez ücret alındı ve para iadesi istiyor lütfen yardım",
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
            "route/unknown latin {label}"
        );
    }
    // and the reason must say what it actually routed on, not report a
    // language it did not identify
    assert!(
        r.route(
            &json!("Müşteriden iki kez ücret alındı ve para iadesi istiyor"),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route")
        .reason
        .contains("not identified"),
        "route/undecided reason mentions letters"
    );
    assert_eq!(
        r.route(
            &json!("Please refund the duplicate charge on invoice 4411 today."),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route")
        .model,
        "english",
        "route/english still english"
    );
    // upstream pins the exact reason on this branch too (test_router.py
    // `route/english reason unchanged`).
    assert_eq!(
        r.route(
            &json!("Please refund the duplicate charge on invoice 4411 today."),
            Some(&empty_questions()),
            &RouteOptions::default(),
        )
        .expect("route")
        .reason,
        "English Latin text",
        "route/english reason unchanged"
    );
    assert_eq!(
        r.route(
            &json!("refund me"),
            Some(&empty_questions()),
            &RouteOptions::default()
        )
        .expect("route")
        .model,
        "english",
        "route/short english still english"
    );
}

// Undecided Latin text follows `default`, as a state with no letters already
// did. Short messages made only of content words carry nothing that names
// their language, and hard-coding English for them sent every short
// Portuguese message to the checkpoint that is 0.97 confident at 0.47
// accuracy on `pt`, whatever the router was configured with.
#[test]
fn undecided_follows_the_default() {
    let r_ml = Router::configure(RouterOptions {
        default: Some("multilingual".to_string()),
        ..RouterOptions::default()
    })
    .expect("router");
    let r_lat = Router::new().expect("router");
    for text in [
        "Quero cancelar",
        "Esqueci minha senha",
        "Fui cobrado duas vezes",
        "Produto veio quebrado, quero trocar",
        "refund me",
    ] {
        assert_eq!(
            r_ml.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "multilingual",
            "route/undecided follows default {text:?}"
        );
        assert_eq!(
            r_lat
                .route(
                    &json!(text),
                    Some(&empty_questions()),
                    &RouteOptions::default()
                )
                .expect("route")
                .model,
            "english",
            "route/undecided stock default {text:?}"
        );
    }
    assert!(
        r_ml.route(
            &json!("Esqueci minha senha"),
            Some(&empty_questions()),
            &RouteOptions::default()
        )
        .expect("route")
        .reason
        .contains("using default (multilingual)"),
        "route/undecided reason names the default"
    );
    // identified English is not undecided, so a non-English default leaves it
    // alone
    for text in [
        "Please refund the duplicate charge",
        "Please refund the duplicate charge on invoice 4411 today.",
    ] {
        assert_eq!(
            r_ml.route(
                &json!(text),
                Some(&empty_questions()),
                &RouteOptions::default()
            )
            .expect("route")
            .model,
            "english",
            "route/identified english ignores default {text:?}"
        );
    }
    assert_eq!(
        r_lat
            .route(
                &json!("Please refund the duplicate charge on invoice 4411 today."),
                Some(&empty_questions()),
                &RouteOptions::default(),
            )
            .expect("route")
            .reason,
        "English Latin text",
        "route/english reason unchanged"
    );
}

// ------------------------------------------------------ unlisted scripts
// `detect_script` counts an alphabetic character only when one of the
// script ranges claims it. An unclaimed character used to be counted
// nowhere, so text written only in such a script produced a total of 0, was
// reported as "unknown", and `analyse` treats "unknown" as English — routed
// to the English checkpoint, which has no tokens for it at all, with the
// reason "no letters detected in state" for text that plainly has letters.
#[test]
fn unlisted_scripts_route_multilingual() {
    for (label, text) in [
        ("halfwidth katakana", "ｱﾘｶﾞﾄｳ"),
        ("bopomofo", "ㄆㄇㄈㄉ"),
        ("kana supplement", "\u{1B000}\u{1B001}"),
        ("CJK Ext-B", "\u{20000}\u{20001}"),
        ("hangul jamo ext-A", "\u{A960}\u{A961}"),
        ("Cherokee", "ᏣᎳᎩ"),
        ("Mongolian", "ᠮᠣᠩᠭᠣᠯ"),
        ("Syriac", "ܫܠܡܐ"),
        ("Thaana", "ދިވެހި"),
        ("Tifinagh", "ⵜⴰⵎⴰⵣⵉⵖⵜ"),
        ("Yi", "ꆈꌠ"),
    ] {
        assert!(
            !analyse(&json!(text)).is_english,
            "unlisted/{label} is not called English"
        );
        assert_eq!(
            route_text(text).model,
            "multilingual",
            "unlisted/{label} routes to multilingual"
        );
    }
    // Fullwidth Latin is Latin, not an unlisted script.
    assert_eq!(
        detect_script("ＨＥＬＬＯ"),
        "latin",
        "unlisted/fullwidth latin is latin"
    );

    // A state with no letters at all must keep behaving exactly as before.
    for (label, text) in [
        ("empty", ""),
        ("digits only", "12345 67890"),
        ("emoji only", "😀😀😀"),
    ] {
        assert_eq!(
            analyse(&json!(text)).script,
            "unknown",
            "unlisted/letterless {label}"
        );
        assert_eq!(
            route_text(text).model,
            "english",
            "unlisted/letterless {label} keeps the default"
        );
    }

    // The scripts the table does name must be untouched.
    for (label, text, script) in [
        ("english", "please cancel my subscription", "latin"),
        (
            "german",
            "Mein Konto wurde zweimal belastet, bitte erstatten Sie den Betrag",
            "latin",
        ),
        ("hindi", "यह एक हिंदी वाक्य है", "devanagari"),
        ("chinese", "请取消我的订阅", "han"),
        ("japanese", "ありがとう", "kana"),
        ("korean", "감사합니다", "hangul"),
        ("russian", "Мой аккаунт был списан дважды", "cyrillic"),
        ("arabic", "تم خصم حسابي مرتين", "arabic"),
        ("greek", "Η χρέωση έγινε δύο φορές", "greek"),
        ("armenian", "Իմ հաշիվը գանձվել է երկու անգամ", "armenian"),
    ] {
        assert_eq!(
            detect_script(text),
            script,
            "unlisted/regression {label} script"
        );
    }
}
