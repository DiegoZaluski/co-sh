# Review consolidado — `/agents` pane coordination & worktree isolation

**Escopo:** commit `b619b415` ("feat: mutual pane awareness and opt-in worktree isolation"), fases 1–6 do `TODO.md` (fase 0 excluída).
**Método:** 2 agentes exploradores baratos (Devin, Kilo) para mapeamento; 6 revisores independentes, um por fase, cada um em seu próprio git worktree; implementação dos issues em paralelo por um 7º agente.
**Base:** sessão sobre `main` em `b619b415`; os fixes pós-review já estão em `main` (`fd5f30fd`, `428b97cb`, `c2e3ed59`, `a3e2a559`).
**Nenhum squash foi feito.**

---

## 1. Veredito por fase

| Fase | Veredito | Gravidade |
|---|---|---|
| 1 — Mutual awareness | Entregue e ligado ponta a ponta, mas o contrato de anúncio é mais fraco do que o plano promete: no máximo uma vez com janela de perda, refirando em conjuntos inalterados, e o peer block vaza exatamente os ids que sua própria doc diz proibir. | **2 MAJOR** |
| 2 — Worktree engine | Núcleo pequeno, legível, sem shell (todos os args via `Command::args`) e testado sobre repos git reais. Porém metade é encanamento morto (`list`/`cleanup` sem chamador de produção), com um bug de correção (path relativo) e um bug destrutivo (`cleanup` apaga worktrees vivos). | **3 MAJOR** |
| 3 — `/worktree` | O encanamento está certo (a cauda de argumentos não se perde; a resolução de cwd alcança dispatch ao vivo e replay). Os defeitos reais: pin em pane que ainda não rodou não é persistido, `add` num pane já pinado órfã o checkout anterior, e o caminho do manager engole a linha digitada sem feedback. | **4 MAJOR** |
| 4 — `agents_worktree` tool | A metade do registry (`WorktreeOp`/`ops`/`peek`/`pop`/`complete`, invariante de temp-dir, limpeza no `set_panes`) está correta e bem documentada. A metade do App não está pronta: a caixa de confirmação só resolve no caminho de teclado — no mouse ela trava a op para sempre e num dos casos **fecha o app**. | **2 BLOCKER + 2 MAJOR** |
| 5 — UI surfacing | Badge pintado corretamente na linha de rodapé reservada sem tocar no layout, e o espelho do painel direito *de fato* mostra o pin (contrariando o explorador) — via texto de descrição. Os problemas graves são staleness: os caminhos de worktree nunca re-sincronizam o espelho e o sync não consegue **limpar** um badge obsoleto após `/worktree exit`. | **2 MAJOR** |
| 6 — Awareness deltas | **Não implementada — e corretamente pulada.** Para panes ACP é impossível sem destruir trabalho (não há canal mid-turn); para o manager já é efetivamente live. O que falta é registrar o *porquê*. | **0 (docs)** |

---

## 2. Problemas por severidade

### BLOCKER

**B1 — A caixa de confirmação do `agents_worktree` é ignorada no mouse: clicar "Yes" FECHA O APP**
`src/tui/app/mouse.rs:1483-1509` não consulta `pending_worktree_confirmation`. Com o Confirm de worktree na tela, clique em "Yes" (`selected == 0`) cai em `resolve_forget_key_confirmation` → `pending_delete.take()` → `handle_rag_confirm_delete`, todos `None` → `self.should_quit = true` (`:1500`) e `dialog.pop()`. O app encerra com a op não resolvida. Clique em "No" (`:1503-1506`) limpa estado alheio e faz `dialog.pop()`: a op fica `Some` **sem diálogo na tela**.
A afirmação de "modal soberano" em `confirm.rs:38-39` só vale para teclado. Mouse é entrada de primeira classe no resto do codebase.
*Direção:* rotear `DialogAction::Confirmed` por `resolve_pending_worktree_confirmation`, como `confirm.rs` faz, antes dos ramos de delete/quit.

**B2 — `DialogAction::Dismissed` vaza a fila de ops permanentemente (head-of-line block)**
`src/tui/app/mouse.rs:1680-1710`: o Confirm devolve `Dismissed` quando o clique cai fora da caixa (`src/tui/ui/dialogs.rs:1101-1106`), e esse ramo termina em `dialog.pop()` sem tocar na op pendente. A op já foi **removida do registry** em `agents_dispatch.rs:1365-1370`, então `set_panes` nunca a limpa e `serve_agents_pane_worktree_ops` retorna para sempre em `agents_dispatch.rs:1310-1312`.
Resultado: bloqueio permanente e silencioso de toda op de worktree posterior, pelo resto da vida do processo; leak de estado do App; loop requisitante que nunca recebe o desfecho.
*Direção:* `resolve_pending_worktree_confirmation(false)` como funil único de qualquer caminho que possa fechar aquele diálogo (ou recusar fechar enquanto houver op estacionada).

### MAJOR

**Fase 1**

- **Peer block viola seu próprio invariante de "não addressability" e duplica o nome do agente** — `src/harness/panes.rs:851`. Renderiza `"- {agent} — {id}"`, imprimindo o id de registry endereçável de cada irmão — exatamente o que sua doc em `:825-828` proíbe ("A pane must never learn the `agents_call` ids"). Evidência viva, repetida cinco vezes nesta sessão: `- kilo — kilo`, `- devin — devin` (nome duplicado, parece saída quebrada). *Direção:* remover o id, renderizar agente + worktree; testar ausência de `#`, não só do próprio nome.
- **Anúncio consumido antes do envio: perda silenciosa** — `agents_dispatch.rs:1143-1148` + `panes.rs:901-902`. `take_preamble` grava `announced[id] = generation` ao *montar* o input, antes de `spawn`/`acp::call`. Se o turno falhar, o anúncio é registrado como entregue e nada reenvia: aquele pane fica sem saber dos irmãos pelo resto da sessão. *Direção:* espiar na montagem, confirmar a geração só no início bem-sucedido do turno.

**Fase 2**

- **Path explícito relativo é armazenado como digitado** — `crates/cosh-tools/src/worktree.rs:196-205`. `explicit.to_path_buf()` sem absolutizar/canonicalizar, enquanto o git resolve contra `current_dir(repo)` (`:139`). `git worktree add ./relx` cria `<repo>/relx`, mas `PaneWorktree.path` guarda `./relx`, que `agents_dispatch.rs:832`/`:1551` entregam como cwd de sessão/processo — resolvido contra o cwd do TUI. Efeito: probe, cwd da sessão ACP e raiz do sandbox fs caem em árvore diferente ou inexistente. Pior: com o parsing por `split_whitespace`, um typo cria checkout *dentro* da árvore do usuário. *Direção:* absolutizar contra `repo` antes de armazenar e antes do teste de existência.
- **`cleanup` apaga worktrees VIVOS via comparação de path não canônica** — `worktree.rs:275` + `:282`. Compara paths de `git worktree list` (canonicalizados) contra `read_dir(root)` crus. Um worktree criado através de raiz simbólica é reportado pelo git já resolvido, enquanto o scan devolve o spelling do symlink → `!registered.contains(path)` verdadeiro → `remove_dir_all` em checkout vivo. Qualquer componente simbólico em `temp_dir()` (macOS `/tmp`→`/private/tmp`, `TMPDIR` simbólico, bind mounts) faz o cleanup apagar todos os worktrees registrados. *Direção:* comparar paths canonicalizados e nunca remover um dir com registro no git.
- **Testes varrem a raiz compartilhada real `/tmp/cosh-worktrees`** — `worktree.rs:392-402` (`sweep_shared_root`). Um `cargo test` apaga, da raiz real, todo dir chamado `feat`, `listed`, `cleanup-test`, `fix-issue` (ou `-<dígitos>`) — slugs plausíveis de branches reais de usuário. O mutex em processo (`:378-384`) não protege entre processos. **Confirmado na prática nesta sessão:** os 6 worktrees de review vivem nessa mesma raiz; sobraram os dirs `listed` e `feat` de resíduo. *Direção:* slug único por `std::process::id()`+contador com guard de limpeza, ou `worktrees_root()` honrando override de env usado pelos testes.

**Fase 3**

- **Pin em pane que nunca rodou não é persistido** — `agents_split.rs:309-321` + `agents_dispatch.rs:408-424`. `to_ref()` devolve `AgentPaneRef::Name(agent)` quando `session_id` é `None`, e `Name` (`src/types.rs:11-14`) não tem campo de worktree. `/worktree add` num pane sem turno não é salvo: quit/reload → pane volta ao cwd da sessão, checkout e branch ficam órfãos no disco. A mensagem do commit afirma que bindings "survive reloads". *Direção:* dar slot de worktree a `Name`, ou recusar `add` até `Bound`.
- **`add` num pane já pinado órfã o checkout anterior silenciosamente** — `agents_dispatch.rs:187-233`. Não checa pin existente: cria segundo worktree, sobrescreve `pane.worktree`, abandona o primeiro sem aviso.
- **Caminho do manager engole a linha digitada** — `agents_split.rs:1169-1183` + `agents_dispatch.rs:99-101`. A entrada sintetizada só existe para não-manager e a rota aborta no guard do manager; mas `SlashMenu::check_state` é agnóstico de fonte, então `/worktree add x` com manager focado mantém o menu aberto sobre o conjunto cosh (sem `worktree`) → "No matching items" → `get_selected_command()` devolve `None` → `keys.rs:2430-2437` engole Enter e a linha é descartada sem prompt nem toast. *(Já corrigido por `428b97cb` + `a3e2a559`; registrado como defeito da entrega original.)*
- **Parsing por `split_whitespace`** — `agents_dispatch.rs:143,188-189`. Path com espaço é inexprimível; `/worktree add a b` lê silenciosamente `path=a branch=b` (o git cria `repo/a`); path relativo diverge entre git e dispatch (ver Fase 2). O toast de uso (`:176`) não menciona nada disso.

**Fase 4**

- **Supremacia é decidida por op, não por época do binding** — `agents_dispatch.rs:1330-1333`. Duas ops enfileiradas para o mesmo pane (a tool devolve recibo na hora, então o modelo pode emitir `assign`+`clear` no mesmo turno; nada limita `enqueue_worktree_op`): a op #1 pergunta ao usuário, ele aprova, o binding vira `AssignedBy::Agent` (`:282`), e a op #2 vê `Agent` e **executa sem diálogo**. O usuário aprovou uma mudança e recebeu duas. *Direção:* uma op em voo por pane, ou carregar o escopo de aprovação no binding.
- **Nenhum caminho de cancelamento para ops estacionadas** — `panes.rs:561-582` (`cancel_requester`) descarta requests de `agents_call` do requisitante parado; `ops` não é tocado. Após Esc o usuário acredita que o turno foi cancelado, mas a ask já estacionada ainda aparece num frame posterior e muda o pane set (com diálogo, possivelmente muito depois do turno do agente).

**Fase 5**

- **`/worktree exit` nunca limpa o badge do espelho** — `commands.rs:287-306` + `right_panel/types.rs:3597-3602`. O caminho de clear passa `label.as_deref().or(mirror.as_deref())`, que é `None` para pane único despinado, e `sync_agents_pane_inner` só escreve `description` quando `label` é `Some`. A linha mantém `⎇ fix-1` para sempre enquanto o rodapé do pane mostra corretamente nada.
- **Nenhum dos quatro caminhos de mudança de binding chama `sync_agents_pane_mirrors`** — `agents_dispatch.rs:220-222`, `:298-301`, `:247-248`, `:328-329`. O assign funciona, registry e metadata atualizam, e a linha do painel direito mantém o texto *pré-assign* até o próximo evento de streaming. Assimetria de ordenação que vai morder todo campo novo adicionado ao espelho.

### MINOR / NIT (seleção)

- `WorktreeApproval` (`panes.rs:148-156`) é **código morto** — nada o constrói; sua doc afirma que a caixa de confirmação "resolve into one of these", mas o App usa `resolve_pending_worktree_confirmation(bool)`.
- Enum de schema vazio quando o split só tem panes manager: `agents_worktree_tool` filtra `!p.is_manager` mas é registrado em `has_panes()` → `"enum": []` e descrição "`pane` is the addressable pane id ()". Vários providers rejeitam enum vazio.
- Desfecho de worktree injetado no transcript com tag `tool: "agents_call"` (`agent_loop.rs:408`, `:1843`) — o TUI e os logs rotulam um desfecho de worktree como `agents_call`.
- `has_outstanding_requests` (`panes.rs:612-623`) não conta ops: o loop não espera o desfecho, então o agente pode encerrar o turno achando que não tem trabalho pendente.
- Bump de `generation` incondicional (`panes.rs:302`) apesar de `PaneDescriptor: PartialEq` — `park_agents_split`/`attach_parked_agents_split` passam conjuntos idênticos e re-armam o preâmbulo de todo pane ACP. 12 sítios de sync.
- `fail_pane` (`panes.rs:553`) muta `state.panes` fora de `set_panes`: sem bump, sem prune de `announced` — o invariante documentado é falso nesse caminho.
- Validação de nome de branch duplicada: `panes.rs:728` reimplementa `worktree::validate_name` (`worktree.rs:123-132`) com outro tipo de erro e outra redação — vão divergir.
- Sem `--` end-of-options: `/worktree add --force <branch>` produz `git worktree add --force -b <branch>` (flag, exit 129). Não é injeção (não há shell), mas é a classe "arg vira flag" que a doc do módulo diz ter fechado.
- Badge com corte duro em `rect.right()` sem reticências e loop manual duplicando `draw_text_line_compact`, ignorando o caminho wide-aware `draw_text_line_wide` (`draw/util.rs:66`) — branch CJK/emoji fica desalinhado. O glifo `⎇` (U+2387) é width-1 e renderiza bem.
- Badge é pintado *antes* do pill de scroll, mesma linha (`agents_split.rs:1752` vs `:1986`) — invisível no estado em que um pane pinado passa mais tempo (rolado durante/após um turno). Documentado como intencional; merece decisão, não precedência silenciosa.
- `PaneWorktree::name` duplica `branch` sem feature de rename por trás (generosidade morta); a UI já trata `name` como canônico.
- `command_arg_tail` (`slash_menu.rs:242-250`) valida `takes_args(cmd_name)` mas nunca checa que a parte-nome do input é realmente `cmd_name` — latente.
- Worktree path chega a `acp::call`/`acp::replay` sem `exists()` (`agents_dispatch.rs:832`, `:1551`): um worktree removido fora de banda transforma todo turno em erro de cwd, sem dica de que `/worktree exit` é o conserto.
- Flag de handoff é limpa no *spawn*, não no sucesso (`agents_dispatch.rs:900-908`): turno que morre já consumiu o resumo.
- Typos/ruído: "unexcutable" (`worktree.rs:87`), `#[must_use]` decorativo em `worktrees_root()`.

---

## 3. Overengineering e duplicação

Este é o eixo mais acionável do review, e o projeto explicitamente rejeita overengineering.

1. **O listing de panes é formatado em TRÊS lugares** — `context_block` (`panes.rs:794-822`, ~29 linhas), `peer_block` (`:833-857`, ~25), `take_preamble` (`:876-897`, ~22). Já divergiram: `id — agent` vs `agent — id`, e "You do NOT coordinate them (no `agents_call` for you)" vs "You do NOT coordinate them". A cópia em `take_preamble` é forçada pelo lock (segura o mutex e não pode chamar `peer_block` → `list_panes` → relock) — pressão real de design, e exatamente por isso o listing deveria ser um `fn render(panes: &[PaneDescriptor], own: Option<&str>)` privado sobre uma slice, com três wrappers finos. Colapsa a duplicação, a inconsistência e o drift de uma vez. *(A fase 6 teria adicionado uma quarta cópia; pular a fase manteve em três.)*
2. **Quatro implementações quase idênticas de assign/clear** — `assign_focused_pane_worktree` (`:187-233`) / `assign_pane_worktree_for_agent` (`:269-314`) e os dois `clear`. Diferem só em como o índice do pane é obtido e na constante `AssignedBy`; as strings de toast são cópias verbatim ("Same side effects as the assign twin" é admissão do próprio autor).
3. **Toasts de sucesso** — 4 sites de happy-path (2× `Success`, 2× `Info`) duplicando o que o badge já diz; a mesma frase de 130 caracteres sobre "the worktree and branch were kept" copiada em `:251-252` e `:332-333`. Corrigido em `c2e3ed59`.
4. **`list`/`cleanup` sem chamador de produção** — ~110 linhas + 4 testes de maquinaria inalcançável, incluindo o `cleanup` destrutivo da Fase 2. A fase entrega 534 linhas onde ~250 carregariam o mesmo comportamento.
5. **Loop de sufixo `-2`/`-3` praticamente inalcançável** — como o branch é sempre `<name>`, um segundo `create` com o mesmo nome morre no ref do branch. O loop só dispara com dois branches distintos que slugificam igual, e nesse caso `name`/`branch` deixam de casar com o diretório.
6. **String do badge composta duas vezes** (rodapé vs espelho), com dois formatos (`"⎇ {name}"` vs `"{mirror} ⎇ {name}"`), e o mapeamento `.name.clone()` escrito duas vezes (`commands.rs:248-251`, `:295`). Um `PaneWorktree::badge()` impediria o drift — que já existe.
7. **Fixture de repo git de brinquedo existe três vezes** — `worktree.rs:297-305`, `commands.rs:2976-2994`, e blocos avulsos em `commands.rs:3146`.
8. **`requester: &'static str` em `WorktreeOp` é inalcançável como `MANAGER`** — `agents_worktree` é removida do loop do manager *e* recusada em `dispatch.rs:265-276`; só `MAIN` estaciona ops.
9. **Placement do engine** — `crates/cosh-tools/src/worktree.rs` não é uma tool (sem schema, sem `ToolDescription`, nunca registrado); seus únicos consumidores são o código TUI do crate binário e a persistência em `src/types.rs`. Pior: o repo já tem política oposta documentada em `src/tui/util/git.rs:1-27` ("do NOT shell out to git: slow, may be missing" — ler `.git/HEAD`), e o engine não pode reusar `find_git_dir` porque ele vive no crate binário. Duas estratégias contraditórias para a mesma pergunta, em lados opostos da fronteira de crate. *Direção:* `crates/cosh-tools/src/util/` ao lado de `util/path_guard.rs`, ou um `cosh-git` mínimo compartilhado.

---

## 4. Testes — cobertura e lacunas

**Bom:** engine sobre repos reais (slug, validação, create com verificação de `.git`/HEAD, dirty-vs-clean remove, list, cleanup); contrato de argumentos do menu nos dois sentidos (`slash_menu.rs:607-663`); `take_preamble` exercitando remoção/re-adição; badge do rodapé assertando `⎇ fix-1` na linha `bottom()-1` e vazio quando despinado; 6 testes de `/worktree` passando; 17 testes de worktree após `a3e2a559`.

**Lacunas, em prioridade:**
1. **Nada exercita o caminho de RECEBIMENTO do diálogo** (`handle_confirm_dialog_key`/`resolve_confirm_dialog`/mouse para op estacionada) — é exatamente onde vivem B1 e B2.
2. **Nada cobre `DialogAction::Dismissed`** nem qualquer fechamento de diálogo com op estacionada.
3. **Nada cobre ops empilhadas** nem worktree `Agent` sendo trocado sem diálogo (a metade `Agent` da decisão 3 não tem teste).
4. **Nada cobre `assign` sobre worktree `User`-assigned** (só `clear` é coberto).
5. **Nada cobre o espelho do painel direito** — metade da Fase 5 sem cobertura, razão pela qual os dois bugs de staleness passaram.
6. Nada cobre remoção de pane entre park e resposta no nível do App; `cancel_requester`/Esc contra op estacionada; ausência de `agents_worktree` na lista de tools de um pane loop ou com `has_panes()` falso; que o schema não tem `path`; path relativo; re-`add` em pane pinado; cwd do replay.
7. **Teste tautológico:** `commands.rs:3206-3224` (handoff) asserta `pending`, depois seta `true` de novo e não asserta nada.
8. **Flakiness/poluição alta:** três testes usam a raiz real compartilhada com mutex apenas intra-processo; um deixa `/tmp/cosh-worktrees/feat` permanentemente; o sweep pode apagar worktrees reais alheios. O teste do lado TUI (`commands.rs:3082`) usa slug único por PID e `remove` explícito — o padrão seguro já era conhecido neste commit.

---

## 5. Os três issues — desfecho

| # | Issue | Status | Commit |
|---|---|---|---|
| 1 | Reutilizar branch existente não vinculado a worktree | **Corrigido.** `branch_exists()` sonda `refs/heads/<name>`; existente e livre → `git worktree add <dir> <branch>`; ocupado → recusa verbatim do git. 2 testes novos. | `fd5f30fd` |
| 2 | `/worktree` ausente no agent pane do Cosh | **Corrigido.** Causa raiz tripla: `slash_source()` devolvia `None` para `MANAGER_LABEL`; `cosh_commands()` não tinha a entrada; `route_agent_slash_command` abortava no guard do manager (linha virava prompt comum). Além disso o manager não honrava `pane.worktree` como cwd. Agora: entrada única no menu, rota aplica, e um helper `pane_effective_cwd` alimenta cwd do turno ACP, `HarnessAssembly::cwd` do manager, probe de snapshot e replay. 5 testes novos. A tool `agents_worktree` continua recusando panes cosh (decisão deliberada: rebind do coordenador é chamada do usuário). | `428b97cb` + `a3e2a559` |
| 3 | Excesso de toasts de sucesso | **Corrigido.** 4 toasts de happy-path removidos; só recusas (`Error`) e uso (`Warning`) permanecem. | `c2e3ed59` |

**Observações sobre o processo de implementação:** o agente 7 original desapareceu do split durante o trabalho (após `fd5f30fd`); um stand-in interno falhou duas vezes sem relatório, deixando trabalho não commitado; o código foi recuperado, verificado — `cargo check -p cosh --lib` passava isolado, mas **`--bin cosh`** revelou um use-after-move real no caminho do manager — corrigido e commitado. O agente 7 também introduziu `428b97cb` afirmando que a entrada do menu resolvia o issue — não resolvia, o comando continuava recusado: **um commit que não entrega o que o título afirma**, o que só apareceu porque o dono do projeto re-testou manualmente.

---

## 6. Teste da funcionalidade de worktree (relato de campo)

Os 6 revisores trabalharam simultaneamente em 6 worktrees isolados, cada um em branch própria (`review/phase-N`) sobre `b619b415`. Todos relataram isolamento efetivo: `pwd` correto, `git status` limpo, `.git` como ponteiro para `/home/inky/co-sh/.git/worktrees/<nome>`, nenhum vazamento para a árvore principal, sem confusão de detached HEAD ou contenção de index.

**O que funcionou:** o binding por pane e a troca de cwd (mecanismo da Fase 3) são sólidos; os revisores rodaram builds e leituras dentro do próprio checkout sem interferência cruzada; `git worktree` se comporta corretamente a partir de um checkout linkado.

**Defeitos na própria funcionalidade, expostos pelo uso:**

1. **`agents_worktree` (a tool do manager) não conseguiu fazer o que a tarefa pedia.** Era o caminho natural para pinar os 7 panes, e a primeira tentativa foi recusada: *"`agents_worktree` cannot bind a cosh pane"*. Todos os panes desta sessão rodam cosh (manager), então a feature é inutilizável para eles — pela tool e, na entrega original, também pelo slash command. Os worktrees tiveram de ser criados manualmente. **Este é o mesmo defeito central do Issue 2**, encontrado de forma independente pelo orquestrador na primeira ação da sessão, antes de qualquer review. Confirma o diagnóstico do dono do projeto: a feature estava efetivamente disponível só para panes ACP/CLIs externos.
2. **`worktrees_root()` é literalmente `/tmp/cosh-worktrees`** — a mesma raiz onde os 6 sandboxes de review vivem. O conjunto de testes da própria feature varre essa raiz por slug fixo (`feat`, `listed`, `cleanup-test`, `fix-issue`) e faz `remove_dir_all`. Um `cargo test -p cosh-tools` de dentro de um sandbox poderia apagar o checkout de um irmão. Resíduo real observado: os diretórios `listed` e `feat` na raiz, com `.git` apontando para tempdir morto. **Autoinfligido e evitável**: os revisores tiveram de ser instruídos a não rodar os testes da feature.
3. **Custo de build por worktree:** sem `target/`, um `cargo check` frio levou de 3m30s a 6m35s (até 17m52s de CPU) e 2,8 GB por sandbox. Sete sandboxes independentes é um multiplicador grande; um `CARGO_TARGET_DIR` compartilhado ou sccache por repo resolveria.
4. **O `TODO.md` é untracked e portanto está AUSENTE em todo worktree** — o plano que o agente implementador é instruído a seguir não existe na árvore onde ele trabalha. Todos os revisores tiveram de ler `/home/inky/co-sh/TODO.md`, furando o isolamento na única direção que importava.
5. **Drift silencioso:** os 7 worktrees ficaram em `b619b415` enquanto `main` avançava para `a3e2a559`. Para review (sabendo qual commit é autoritativo) é aceitável; para um agente *implementador* significaria construir sobre base obsoleta sem aviso. O revisor da fase 5 notou exatamente isso ("4 commits behind").
6. **O mecanismo de persistência tem dois leaks confirmados em uso:** binding em pane que nunca rodou não é salvo (Fase 3), e o badge do espelho não é limpo no `exit` (Fase 5).

**Conclusão operacional:** o isolamento em si funciona bem e o teste valeu a pena — os defeitos acima só apareceram em uso real, não teriam aparecido em revisão estática. Mas o uso simultâneo expôs o item 2 como risco de perda de dados, e os itens 1/4 mostram que a feature foi projetada para panes ACP e o caso cosh ficou como cidadão de segunda classe.

---

## 7. Investigação adicional — o acoplamento worktree ↔ `agents_call`

### A premissa precisa de correção

A pergunta parte de "a decisão de acoplar a funcionalidade de worktree à tool `agents_call`". **A premissa não corresponde ao código.** No commit `b619b415`, worktree **não** está acoplado a `agents_call`: é uma **tool separada e independente**, com nome, descrição, schema e braço de dispatch próprios.

Evidência:

| Aspecto | `agents_call` | `agents_worktree` |
|---|---|---|
| Definição da tool | `panes.rs:912-950` | `panes.rs:959-1016` |
| Braço no dispatch | `dispatch.rs:150` | `dispatch.rs:242` |
| Schema de entrada | `{pane, input, run_in_background}` | `{pane, action, name?}` |
| Registro | `build.rs:119` | `build.rs:121-123` |
| Remoção em pane loop | `build.rs:363` | `build.rs:368` |
| Resolução em extraction | `extraction.rs:122` | `extraction.rs:127` |

Ou seja: a separação que a investigação sugere como alternativa melhor **já é o que existe**. A hipótese "uma tool independente `agents_worktree` parece uma separação mais adequada" está implementada, e isso foi especificado pelo próprio plano (Phase 4: "New harness tool `agents_worktree` in `src/harness/panes.rs`").

### O que realmente é compartilhado, e se está documentado

O acoplamento real é de **módulo e de contrato de infraestrutura**, não de tool:

1. **Mesmo módulo** (`src/harness/panes.rs`) — decisão do plano.
2. **Mesmo gate de registro** (`has_panes()`) e mesma remoção em pane loops (`with_own_pane_id`).
3. **Mesma maquinaria de entrega** — `PaneDelivery`, `take_ready_deliveries`, `delivery_note`, tagging por requester; `DeliveryKind` é uma adição de 2 variantes, não um fork.
4. **Mesmo namespace de ids** — "the same addressable ids `agents_call` speaks", restrito aos não-manager.

**Sim, existem justificativas documentadas**, e são substanciais:

- `panes.rs:952-959` documenta a tool: por que compartilha os ids, por que só registra com panes, por que é removida de pane loops ("the user's supremacy tooling"), e por que worktrees ficam opt-in.
- `dispatch.rs:232-241` documenta o contrato de parking: a tool **nunca toca git**, valida na hora para o modelo se corrigir, estaciona a op, e o desfecho **sempre** chega como delivery — com a razão explícita ("an ask may sit across frames waiting for the user's confirmation box, and a receipt cannot").
- `panes.rs:680-697` documenta `enqueue_worktree_op` como "the mirror of `enqueue`'s contract, minus the wait-mode half", enumerando os casos de recusa.
- `panes.rs:157-178` e `:215-220` documentam `WorktreeOp` e a fila `ops`.
- `panes.rs:94-101` documenta `DeliveryKind` como "an `agents_call` pane report or a finished `agents_worktree` op. The drain and the requester tagging are shared; the note format differs per kind".

### Veredito: deliberado, não acidental

É claramente **deliberado**, com três marcas de intenção:

1. A separação em tools foi **especificada no plano** antes da implementação.
2. O contrato de parking espelha conscientemente `enqueue` "minus the wait-mode half", com a razão (a op pode atravessar frames esperando o usuário) explicitada — um acoplamento de *padrão*, articulado.
3. A revisão da Fase 4 concluiu que o compartilhamento é **justificado e não deveria ser fundido**: dobrar ops em `queues`/`serving` (chaveados por pane, um turno por pane, oneshot de wait-mode) seria o reuso errado, porque ops executam no frame do App, não num turno de pane.

**Por que a leitura de "acoplamento inadequado" é tentadora:** o módulo se chama `panes.rs`, seu header (`:1-26`) descreve **apenas** a ponte `agents_call`, e a descrição da tool `agents_call` nunca menciona worktree. Quem lê o header conclui que o módulo é sobre mensageria entre panes e que o worktree é um enxerto. Também contribui o nome: `agents_worktree` "parece" um parâmetro de `agents_call` por compartilhar o prefixo `agents_`.

### O que merece correção (achados legítimos embutidos na suspeita)

1. **O header do módulo (`panes.rs:1-26`) não menciona o segundo consumidor.** Documenta a ponte `agents_call`, o ownership split e o tagging de requester, mas nada sobre a fila de ops, seu contrato "never waits" nem o `DeliveryKind::Worktree`. *Correção:* 5–8 linhas no header. É a lacuna de documentação real, e a origem da suspeita.
2. **`requester: &'static str` em `WorktreeOp` é inalcançável como `MANAGER`** — `agents_worktree` é removida do loop do manager *e* recusada em `dispatch.rs:265-276`, então só `MAIN` estaciona ops. O campo e o roteamento `MANAGER` de ops são generalidade morta — overengineering, pela régua do projeto.
3. **`context_block` (`panes.rs:~795`) diz "address each by its pane id in `agents_call`"**, texto anterior ao segundo consumidor dos mesmos ids.
4. **Duplicação de construção de schema** entre as duas tools (`enum_values`/`pane_list` montados duas vezes) e dois `harness_tools.push` consecutivos em `build.rs:119-123` onde um helper serviria.

### Uma tool independente seria melhor?

**Não se aplica como reformulação — a tool já é independente.** A pergunta certa é se o **compartilhamento de módulo e maquinaria** deveria ser desfeito. Resposta: **não**. As razões são concretas:

- Ambas são tooling de coordenação de panes, sujeitas ao mesmo gate (`has_panes()`), ao mesmo strip em pane loops e ao mesmo modelo de autoridade (usuário > agente). Duplicar o gate e o strip criaria duas fontes de verdade para um invariante de segurança.
- Ambas entregam desfecho assíncrono pela mesma drain, com o mesmo tagging por requester; `DeliveryKind` já é a extensão mínima e correta.
- Um módulo novo (`pane_worktrees.rs`) duplicaria o acesso ao registry singleton ou exigiria expor `RegistryState` — pior acoplamento, não melhor.

O que **seria** uma melhoria de separação, e é independente desta discussão: **tirar o engine git de `crates/cosh-tools/src/worktree.rs`** (item 9 da seção 3). Ele não é uma tool, seus consumidores são o crate binário, e ele contradiz a política documentada em `src/tui/util/git.rs`. Essa é a separação que falta — não a das tools.

---

## 8. Recomendação final

**Antes de qualquer merge:** corrigir B1 e B2 (mouse + dismiss da confirmação). São perda de trabalho do usuário e um quit inesperado, e vivem no caminho que a feature mais vai usar.

**Logo depois, em ordem de risco:** os 3 MAJOR da Fase 2 (path relativo, `cleanup` destrutivo, testes varrendo a raiz compartilhada — este último é risco de perda de dados de terceiros); os 2 MAJOR da Fase 5 (badge obsoleto no espelho); o pin não persistido e o `add` que órfã checkout (Fase 3); e o par supremacia/cancelamento (Fase 4).

**Refatoração que paga por si:** unificar a formatação do listing (3 cópias divergentes), unificar assign/clear (4 cópias verbatim), e remover o que é morto (`WorktreeApproval`, `list`/`cleanup` sem chamador, loop de sufixo, `requester` de `WorktreeOp`).

**Docs:** header de `panes.rs` descrevendo o segundo consumidor; registrar por que a Fase 6 foi pulada; corrigir o drift em `peer_block:825-831` (diz Fase 3 onde é 1d, e aponta para `peer_block` onde ACP usa `take_preamble`).

**Nenhum squash foi feito.** Os commits `fd5f30fd`, `428b97cb`, `c2e3ed59`, `a3e2a559` permanecem como commits normais, aguardando instrução.
