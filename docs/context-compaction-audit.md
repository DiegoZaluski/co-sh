# Auditoria das fronteiras da sumarização — 2026-09-13

Escopo: sumarização normal, seleção de modelos, entrada em contingência,
MapReduce, retomada do split legado, aplicação do checkpoint e eventos da TUI.
Esta é uma revisão desse subsistema, não uma auditoria de todo o repositório.

## Responsabilidades

- O gatilho do agente decide **quando** sumarizar.
- O conector do sumarizador define os parâmetros de geração. A sumarização
  normal preserva esses parâmetros, sem instalar um teto de segmento.
- A janela do sumarizador determina quais **requisições** ele consegue receber.
- O orçamento do agente determina o trecho recente preservado e se o
  **contexto resultante** pode ser aplicado.
- MapReduce é uma contingência para requisições que não cabem, além da retomada
  de progresso existente. Seus limites de segmento não regem o fluxo normal.

## Problemas corrigidos

### 1. Teto de segmento aplicado ao resumo completo

O fluxo normal impunha `max_tokens = clamp(window / 10, 64, 2000)` quando havia
descoberta de janela. A primeira correção substituiu isso por um teto de
16.384; continuava misturando as duas responsabilidades.

Agora o conector é preservado. Sem limite configurado e sem default obrigatório
do adaptador, nenhum `max_tokens` é acrescentado. Um limite explícito continua
sendo respeitado. Isso não remove limites físicos/defaults do provedor.

Evidência: `normal_summary_preserves_the_connectors_output_setting` falhou
antes, observando `Some(200)` onde o conector tinha `None`. O teste HTTP/SSE com
janela de 1M também exige uma resposta acima dos antigos tetos e verifica que
automático e manual enviam `[None, None]`. As verificações de término natural
permanecem ativas.

### 2. Escolher contingência antes de escolher o sumarizador

No início do turno e após fallback, o harness comparava o contexto com a
janela do agente antes de resolver a lista de sumarizadores. Um agente menor
podia forçar MapReduce mesmo havendo um sumarizador capaz de receber tudo.
Além disso, comparava o contexto inteiro, em vez do prompt realmente enviado.

A seleção agora precede a verificação do prompt completo, incluindo instruções,
overhead e a reserva de saída efetiva do conector. O teste
`automatic_compaction_selects_summarizer_before_testing_its_window` reproduziu
MapReduce desnecessário com agente de 4k e sumarizador de 1M; agora exige uma
única chamada normal, sem eventos de fases do MapReduce.

### 3. Janela do sumarizador alterava o trecho recente do agente

`begin_map_reduce(window)` usava `window` para escolher o histórico coberto,
incluindo a reserva de 20% de contexto recente. Trocar apenas o sumarizador
mudava quais mensagens o agente conservava literalmente.

A cobertura agora usa o orçamento do agente; a janela do sumarizador serve
para empacotar as requisições. Reproduzido por
`map_reduce_retains_the_agents_raw_tail_independent_of_summarizer_window`:
um trecho recente que cabe na reserva do agente de 10k era absorvido pelo
resumo quando o sumarizador tinha janela de 1k.

### 4. Contexto do agente validado contra a janela do sumarizador

O commit do MapReduce exigia que resumo + trecho recente + TODO coubessem em
`min(80% da janela do sumarizador, gatilho do agente)`. O sumarizador não recebe
esse contexto combinado; quem o recebe é o agente.

O commit agora usa o orçamento do agente. O teste
`map_reduce_commit_uses_the_receiving_agents_budget` rejeitava um contexto de
aproximadamente 1.200 tokens para um agente com orçamento de 10k porque o
sumarizador tinha janela de 1k. O mesmo contexto agora é aceito. Rejeições de
contextos que realmente excedem o orçamento do agente continuam cobertas.

### 5. Validar tamanho e depois aumentar o texto

O resumo era medido antes de `summary_annotator` acrescentar o aviso sobre
ferramentas indisponíveis. O commit podia declarar sucesso e instalar um texto
maior que o limite recém-verificado.

A anotação agora acontece antes da medição, uma única vez por tentativa de
commit, e o texto medido é o texto instalado. O teste
`checkpoint_budget_includes_the_annotation_that_will_be_committed` reproduziu
a aceitação indevida e verifica rejeição sem nenhuma mudança de estado.

### 6. Commit legado perdia progresso ao rejeitar

`commit_split` retirava o staging com `take()` antes da validação, sem
restaurá-lo em caso de rejeição. Também checava somente o buffer sem anotação
e confiava no chamador para garantir que todos os itens haviam sido consumidos.

Agora valida o contexto final anotado, exige consumo completo e restaura o
staging rejeitado. A API de commit não pode mais cobrir itens ainda não
sumarizados. Testes: `legacy_commit_checks_annotated_output_and_preserves_rejected_staging`
e `legacy_commit_cannot_cover_items_that_have_not_been_summarized`. A proteção
de consumo completo já existia no fluxo usual do harness; a lacuna era na API
de commit, não uma perda de histórico observada na sessão relatada.

### 7. Renovar tentativas após cada ferramenta

Uma falha mantinha o contexto acima do gatilho e cada dispatch abria um novo
ciclo de três tentativas. A correção anterior foi mantida: falha automática
esgotada adia novas tentativas pelo gatilho até outro turno ou mudança de
modelo. Recuperação por erro real de janela continua independente.

Testes cobrem falhas no início e no meio do turno, próxima interação, comando
manual e novas compactações bem-sucedidas quando o histórico volta a crescer.

### 8. Concatenar respostas de tentativas distintas na TUI

O candidato interno era limpo, mas os fragmentos visíveis eram concatenados.
A sessão persistida continha três títulos `## Objective` em cada caixa que
falhava, e nenhum `## Code & Anchors` nessas caixas.

Cada retry agora reinicia a saída visível. O teste
`truncated_retries_replace_visible_output_and_never_commit` verifica que fica
somente a última resposta e que nenhum fragmento incompleto vira checkpoint.

## Achados adicionais corrigidos

### 9. Redução final usava o mesmo teto de um mapa individual

Em `core.rs`, `summarize_checkpoint_request` usa
`summarize_map_with_connector` para mapas, reduções, auditorias e correções.
Mapas, reduções intermediárias e auditorias continuam limitados para caber nas
etapas seguintes. A redução final e as correções agora preservam a configuração
de saída do conector, sem instalar o teto de 2k. O teste
`final_reduction_and_correction_preserve_output_settings_while_audits_stay_bounded`
verifica uma resposta final maior que 2k e confirma limites apenas nas
auditorias.

### 10. Truncamento era tratado como erro genérico transitório

`validate_summary_completion` agora produz `CompactionErr::Incomplete` para
`length`, `max_tokens`, término ausente e outros sinais incompletos. O pedido
não é repetido: resultados paralelos já iniciados são drenados, mapas aceitos
permanecem persistidos e o próximo modelo configurado pode assumir a etapa.
Os testes `incomplete_parallel_map_drains_successes_without_rescheduling`,
`incomplete_checkpoint_request_does_not_repeat_an_unchanged_request` e
`incomplete_summary_advances_directly_to_the_configured_fallback` fixam essa
política. Erros de transporte continuam sujeitos aos retries limitados.

## Validação

As reproduções do teto de saída, seleção de modelo, cobertura do MapReduce,
orçamento de commit e anotação falharam nos pontos descritos antes das
correções. Os testes HTTP/SSE usam servidor local e o SDK
real; o agente principal usa respostas programadas. Não houve chamadas pagas
nem alteração do histórico local usado na investigação.

Comandos de validação:

```sh
cargo test --lib --no-default-features --quiet
cargo check --lib --no-default-features --quiet
```

Resultado final: 363 testes aprovados, 2 ignorados; `cargo check` aprovado.
