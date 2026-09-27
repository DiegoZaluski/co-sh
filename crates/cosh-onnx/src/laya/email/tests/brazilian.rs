use super::*;

// --------------------------------------------------------------- Brazilian clients and footers
#[test]
fn pt_footer_removed() {
    let boleto = "Preciso da segunda via do boleto.";
    for (label, footer) in [
        ("galaxy", "Enviado do meu Galaxy"),
        ("samsung default, 41 characters", "Enviado do meu smartphone Samsung Galaxy."),
        ("outlook ios", "Enviado do Outlook para iOS"),
        ("outlook android download line", "Obter o Outlook para Android"),
        ("windows mail", "Enviado do Email para Windows"),
        ("yahoo", "Enviado do Yahoo Mail no Android"),
        ("eco footer", "Antes de imprimir, pense em sua responsabilidade e compromisso com o MEIO AMBIENTE."),
        ("eco footer, reversed", "Pense no meio ambiente antes de imprimir este e-mail."),
    ] {
        assert_eq!(
            clean(&format!("{boleto}\n\n{footer}")),
            boleto,
            "pt/footer removed: {label} — got {:?}",
            clean(&format!("{boleto}\n\n{footer}"))
        );
    }
}

#[test]
fn en_outlook_download_line_removed() {
    assert_eq!(
        clean("Please resend the invoice.\n\nGet Outlook for iOS"),
        "Please resend the invoice.",
        "en/outlook download line removed"
    );
}

#[test]
fn pt_outlook_header_without_address_is_cut() {
    // Exchange leaves the address out of the header; the `Enviado:`/dated
    // `Data:` line under it still marks it
    for (label, second) in [
        ("enviado", "Enviado: sexta-feira, 19 de setembro de 2026 10:02"),
        ("data", "Data: sexta-feira, 19 de setembro de 2026 10:02"),
    ] {
        assert_eq!(
            clean(&format!(
                "Segue o comprovante.\n\nDe: Maria Souza\n{second}\nPara: Suporte\n\
                 Assunto: cancelar contrato\n\nQueremos cancelar o contrato."
            )),
            "Segue o comprovante.",
            "pt/outlook header without address is cut: {label}"
        );
    }
}

#[test]
fn pt_requests_that_look_like_footers_are_kept() {
    // ...and none of them may take the request with it
    for (label, body) in [
        ("device words inside a request", "Oi,\nSegue o pedido.\nEnviado do meu celular o comprovante ontem."),
        ("`De:`/`Para:` date range", "Preciso das férias.\nDe: 10/09\nPara: 15/09\nPode aprovar?"),
        ("`De:` + undated `Data:`", "Relatório do evento.\nDe: João\nData: amanhã cedo\nPode confirmar?"),
        ("`antes de imprimir` in a request", "Antes de imprimir o boleto, confira o valor. Está errado."),
        ("`get` + device word", "Hi,\nThe box is at the front desk.\nGet mail"),
    ] {
        assert_eq!(clean(body), body, "pt/kept: {label}");
    }
}

