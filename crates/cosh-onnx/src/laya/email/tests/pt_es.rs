use super::*;

// --------------------------------------------------------------- Portuguese and Spanish mail
#[test]
fn pt_full_reply_keeps_only_the_request() {
    assert_eq!(
        clean(
            "Olá equipe,\n\n\
Fomos cobrados duas vezes na fatura de março. Por favor, estornem a cobrança duplicada hoje.\n\n\
Atenciosamente,\n\
João Silva\n\
Financeiro - ACME Ltda\n\n\
Enviado do meu iPhone\n\n\
Esta mensagem pode conter informações confidenciais. Se você recebeu esta mensagem por engano, favor apagá-la.\n\n\
Em seg., 22 de set. de 2026 às 10:14, Suporte <suporte@x.com> escreveu:\n\
> Olá João, recebemos seu chamado de cancelamento do plano Enterprise."
        ),
        "Olá equipe,\n\nFomos cobrados duas vezes na fatura de março. \
Por favor, estornem a cobrança duplicada hoje.",
        "pt/full reply keeps only the request"
    );
}

#[test]
fn pt_outlook_original_message_block_is_cut() {
    assert_eq!(
        clean(
            "Segue o comprovante do pagamento.\n\n-----Mensagem original-----\n\
             De: Maria <maria@acme.com>\nAssunto: cancelar contrato\nQueremos cancelar o contrato."
        ),
        "Segue o comprovante do pagamento.",
        "pt/outlook original-message block is cut"
    );
}

#[test]
fn pt_outlook_header_without_separator_is_cut() {
    assert_eq!(
        clean(
            "Segue o comprovante.\n\nDe: Maria <maria@acme.com>\nEnviado: segunda-feira\n\
             Assunto: cancelar contrato\nQueremos cancelar o contrato."
        ),
        "Segue o comprovante.",
        "pt/outlook header without separator is cut"
    );
}

#[test]
fn pt_gmail_attribution_wrapped_over_two_lines_is_cut_whole() {
    assert_eq!(
        clean(
            "O acesso voltou, obrigado.\n\nEm seg., 22 de set. de 2026 às 10:14, Suporte Técnico <\n\
             suporte@acme.com> escreveu:\n> texto antigo"
        ),
        "O acesso voltou, obrigado.",
        "pt/gmail attribution wrapped over two lines is cut whole"
    );
}

#[test]
fn pt_short_signoff_is_removed() {
    assert_eq!(
        clean("Bom dia,\nO boleto de março não chegou.\nObrigado,\nAna"),
        "Bom dia,\nO boleto de março não chegou.",
        "pt/short sign-off is removed"
    );
}

#[test]
fn es_reply_keeps_only_the_request() {
    assert_eq!(
        clean(
            "Hola,\nNo puedo acceder a mi cuenta desde ayer.\nSaludos,\nCarlos\n\n\
             El lun, 22 sept 2026 a las 10:14, Soporte <soporte@x.com> escribió:\n> texto anterior"
        ),
        "Hola,\nNo puedo acceder a mi cuenta desde ayer.",
        "es/reply keeps only the request"
    );
}

#[test]
fn es_disclaimer_footer_is_dropped() {
    assert_eq!(
        clean(
            "Necesito la factura de marzo.\n\nSi usted ha recibido este mensaje por error, bórrelo."
        ),
        "Necesito la factura de marzo.",
        "es/disclaimer footer is dropped"
    );
}

// --------------------------------------------------------------- ...without eating the request
#[test]
fn pt_request_mentioning_confidencial_is_kept() {
    assert_eq!(
        clean("Preciso do contrato confidencial assinado até sexta."),
        "Preciso do contrato confidencial assinado até sexta.",
        "pt/request mentioning `confidencial` is kept"
    );
}

#[test]
fn pt_obrigado_opening_a_sentence_is_not_a_signature() {
    assert_eq!(
        clean("Oi,\nRecebi a resposta.\nObrigado pelo retorno, mas continua\nsem funcionar."),
        "Oi,\nRecebi a resposta.\nObrigado pelo retorno, mas continua\nsem funcionar.",
        "pt/`Obrigado` opening a sentence is not a signature"
    );
}

#[test]
fn pt_caso_tenha_recebido_footer_is_dropped() {
    assert_eq!(
        clean(
            "Favor reenviar a nota fiscal.\n\nCaso tenha recebido esta mensagem por engano, \
             notifique o remetente."
        ),
        "Favor reenviar a nota fiscal.",
        "pt/`caso tenha recebido` footer is dropped"
    );
}

#[test]
fn pt_gmail_attribution_wrapped_inside_the_name_is_cut_whole() {
    assert_eq!(
        clean(
            "Resolvido, pode fechar.\n\nEm qua., 24 de set. de 2026 às 09:02, Suporte\n\
             Técnico <suporte@acme.com> escreveu:\n> texto antigo"
        ),
        "Resolvido, pode fechar.",
        "pt/gmail attribution wrapped inside the name is cut whole"
    );
}

#[test]
fn en_wrapped_attribution_is_cut_whole_too() {
    assert_eq!(
        clean(
            "Fixed, thanks.\n\nOn Wed, Sep 24, 2026 at 9:02 AM Support Team <\n\
             support@acme.com> wrote:\n> old text"
        ),
        "Fixed, thanks.",
        "en/wrapped attribution is cut whole too"
    );
}

#[test]
fn pt_em_escreveu_without_a_date_is_body_text() {
    assert_eq!(
        clean("Oi,\nEm resposta ao que você escreveu:\no pedido 4411 ainda não chegou."),
        "Oi,\nEm resposta ao que você escreveu:\no pedido 4411 ainda não chegou.",
        "pt/`Em ... escreveu:` without a date is body text"
    );
}

#[test]
fn pt_destinado_exclusivamente_in_a_request_is_kept() {
    assert_eq!(
        clean("O valor é destinado exclusivamente ao pagamento do boleto. Podem confirmar?"),
        "O valor é destinado exclusivamente ao pagamento do boleto. Podem confirmar?",
        "pt/`destinado exclusivamente` in a request is kept"
    );
}

#[test]
fn pt_de_without_an_address_is_body_text() {
    assert_eq!(
        clean("Preciso das férias.\nDe: 10/09 a 15/09\nPode aprovar?"),
        "Preciso das férias.\nDe: 10/09 a 15/09\nPode aprovar?",
        "pt/`De:` without an address is body text"
    );
}
