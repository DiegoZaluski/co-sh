# TODO Tool — Improvement Ideas

> **Status guide for each idea:**
>
> - **speculative** — No benchmark proves this improves LLM agent task outcomes.
>   The idea is grounded in analogy to human tooling or software design
>   principles, but its effect on an LLM agent is unmeasured.
>
> - **potential** — There is some benchmark evidence (academic paper, released
>   agent eval, or production data from another agent CLI) that this feature
>   improves task completion, correctness, or efficiency. When available, the
>   source is cited.
>
> Ideas that already exist in the tool are omitted.
>
> Implementation effort:  **▲** small  **▲▲** medium  **▲▲▲** large

---

## State & Transitions

### `blocked` status (distinct from `Pending`)
- **speculative**
- Currently a task whose deps are unsatisfied is still `Pending`. Adding a
  `Blocked` status (set automatically when deps link to non-Completed tasks)
  gives the LLM a clearer signal: *don't start this, wait for something else*.
  Could also act as an auto-transition: when all deps → `Completed`, auto
  move `Blocked` → `Pending`.
- Analogous to: Claude Code's implicit blocked state when deps aren't met.
- Effort:  **▲▲**
- Cross-ref: requires dependency graph aware transitions.

### `Ready` status (auto-transition from `Blocked`)
- **speculative**
- A task becomes `Ready` only when all its `depends_on` are `Completed` and
  no other task is `InProgress`. Gives the LLM a clear "pick me" hint.
- Could be computed rather than stored (derived from `Blocked` + dep status).
- Effort:  **▲**

### Soft-delete / archive status
- **speculative**
- `Remove` currently hard-deletes items. A `Deleted` or `Archived` status
  would preserve the record (for resolution history, dependency graph
  integrity) while hiding it from normal listing. Stale-dep nags go away
  naturally since the ref still exists.
- Claude Code uses a `deleted` status (soft tombstone).
- Effort:  **▲▲**

### Re-open / reactivate completed tasks
- **speculative**
- Once `Completed`, there's no way back. LLMs sometimes realize they were
  wrong about a completion. A `Reopen` action moves `Completed` → `Pending`
  (or `InProgress`), clearing rationale and resetting the resolution lifecycle.
- Effort:  **▲**

---

## Hierarchical Decomposition

### Sub-tasks / child tasks
- **speculative**
- A task can have `children: Vec<String>` (IDs of sub-tasks).
  - Parent status is derived from children: `Completed` iff all children
    completed; `InProgress` iff any child in progress; etc.
  - `Clean` on a parent cascades to children (or errors if children exist).
- High value for LLM agents that naturally decompose goals into steps.
- Parallel sub-agent delegation would use this field.
- Effort:  **▲▲▲**
- Not blocked by sub-agent system: the data model can land first; sub-agent
  wiring comes later.

### Auto-checklist generation tool
- **speculative**
- A new action `AddChecklist { goal: String }` that returns a structured
  decomposition of a high-level goal into sub-tasks. The LLM can review,
  modify, and approve before adding.
- Inspired by: Goose's auto-checklist feature when processing multi-file
  requests.
- Effort:  **▲▲**
- Risk: LLM-generated plans may hallucinate steps. Mitigate with review nag.

### Epic / milestone grouping
- **speculative**
- Add an optional `epic: Option<String>` field. Tasks within the same epic
  can be listed and status-rolled-up. Useful for large features that span
  multiple steps.
- Effort:  **▲**

---

## Prioritisation & Estimation

### Priority field (P0–P3 or critical/high/medium/low)
- **speculative**
- Add `priority` to `TodoItem` / `TodoTags` (or new separate field).
- Enables `NextTask` action to return highest-priority actionable task.
- Docker Agent Tasks has priority sorting. Common in human PM tools.
- Effort:  **▲**

### NextTask action
- **speculative**
- Returns the single best task to work on now: highest priority among
  `Pending` / `Ready` tasks, with satisfied deps, that isn't blocked by
  another `InProgress`.
- Saves the LLM from having to scan and reason about the full list on every
  step.
- Effort:  **▲**

### Effort estimate (story points / complexity metric)
- **speculative**
- Add `effort: Option<u32>` (e.g. minutes estimate, or Fibonacci points).
  Useful for the LLM to budget context window and decide order of work.
- No agent CLI yet benchmarks this. Purely speculative.
- Effort:  **▲**

---

## Dependency & Graph

### Bidirectional dependency tracking (`depends_on` + `blocked_by`)
- **speculative**
- From Claude Code: a task stores both what it depends on AND what depends on
  it (`blocks`). Currently cosh only tracks `depends_on` (forward).
- Querying "what does this task block?" currently requires scanning all items.
  A cached reverse index simplifies `GetTask` and is cheap to maintain.
- Effort:  **▲**

### Dependency cycle detection
- **potential**
- cosh currently detects only self-references. A full cycle detection (e.g.
  DFS over the dep graph) would prevent deadlock scenarios.
- This is standard graph theory — no benchmark needed, but it's a correctness
  invariant, not a performance claim.
- Effort:  **▲**

### Auto-remove stale deps on delete
- **potential**
- Currently `Remove` warns about stale refs but doesn't fix them. An optional
  `cascade: bool` parameter could auto-remove the deleted ID from all other
  tasks' `depends_on` lists. Docker Agent does this automatically.
- Verified pattern from production agent tooling.
- Effort:  **▲**

---

## Field Enrichment

### Title / subject + long description split
- **speculative**
- Currently `description` serves both as a short title and as the full
  explanation. Separate into `title: String` (required, short) and
  `body: Option<String>` (optional, arbitrary length).
- Claude Code uses `subject` + `description`. Human PM tools universally
  distinguish.
- Effort:  **▲**

### Free-form labels / tags (multi-value)
- **speculative**
- `TodoTags` currently holds a single boolean `possibly_important`. Replace
  with `tags: Vec<String>` (free-form) while keeping `possibly_important` as
  a derived convenience or separate flag.
- Enables classification like `["bug", "parser", "high-mem"]`. LLMs can
  filter by tag, group, or summarise.
- Effort:  **▲**

### Acceptance criteria field
- **speculative**
- `acceptance: Option<Vec<String>>` — a list of concrete, testable conditions
  that define "done". The LLM can check these before calling `Complete`.
- Reduces premature completion in agents. No benchmark yet.
- Effort:  **▲**

### Uncertainty / confidence score
- **speculative**
- `confidence: Option<f32>` (0.0–1.0) on each task. The LLM annotates how
  confident it is in its approach before starting. When confidence is low,
  the nag system can suggest verification subtasks or human review.
- Emerges from research on uncertainty-aware agent planning (2025 papers).
- Effort:  **▲**

---

## Actions

### GetTask (view single task by ID)
- **speculative**
- Returns full details of one task. Currently the LLM must carry the entire
  list in context. A targeted lookup saves tokens and focus.
- Claude Code and Docker Agent both have this. Standard CRUD operation.
- Effort:  **▲**

### List with filters (by status, priority, tag, epic)
- **speculative**
- `List { status: Option<TodoStatus>, priority: Option<Range>, tag: Option<String> }`
  returns a filtered subset. Reduces context overhead when the task list
  grows beyond a handful of items.
- Docker Agent supports filtering: `list_tasks(status="in_progress")`.
- Effort:  **▲**

### Batch operations (AddMany, UpdateMany, RemoveMany)
- **speculative**
- LLMs often create or complete tasks in groups. A batch action reduces
  tool call overhead and state cloning (clone list once, apply N mutations).
- Docker Agent has `create_todos` (array input).
- Effort:  **▲▲** (requires careful error semantics — all-or-nothing vs
  partial success)

### Reorder / prioritise (move task up/down, set rank)
- **speculative**
- `Reorder { id: String, position: usize }` — explicit ordering beyond
  insertion order. Useful for the LLM to plan execution sequence.
- No agent CLI currently has this.
- Effort:  **▲**

### AddDependency / RemoveDependency (dedicated actions)
- **speculative**
- Currently dep editing requires a full `Update` call with the entire
  `depends_on` vec. Dedicated actions are more ergonomic for the LLM.
- Docker Agent has separate dep management actions.
- Effort:  **▲**

---

## LLM-Specific Guidance

### Verification subtask pattern
- **speculative**
- A nag or convention: after completing a complex task, the LLM should create
  a verification sub-task (e.g. "Verify fix with integration test") before
  marking the original done. Could be enforced at the tool level with a
  `require_verification` tag.
- Research: multi-model routing with dedicated verifier agents shows
  improved correctness in SWE-bench-like evaluations (2025).
- Effort:  **▲**

### Re-planning trigger nags
- **speculative**
- When a task exceeds its `timeline_ms` N times, the nag escalates:
  "This task has exceeded its budget twice. Consider re-planning or breaking
  it down."
- Effort:  **▲**

### Step efficiency tracking (tool calls per task)
- **potential**
- Track `tool_call_count: u64` per task (incremented externally by the
  harness). When a task exceeds a threshold, nag about over-complexity.
- Multiple research papers (TDAG, SWE-agent analysis) use tool-call-per-task
  as an efficiency metric.
- Effort:  **▲▲** (requires harness integration to pass call count)

---

## Persistence & Multi-Agent

### Disk-backed persistence (optional, behind a feature flag)
- **speculative**
- cosh's pure-functional design defers persistence intentionally. An optional
  `persist` feature could add `TodoList::save(path)` / `TodoList::load(path)`
  (JSON or MessagePack). The tool itself stays pure; the caller decides when
  to save.
- Across agent CLIs, persistence is nearly universal (Claude Code file-based,
  Docker Agent session-based, OpenHands DB-backed).
- Effort:  **▲▲**

### Owner / assignee field
- **speculative**
- `owner: Option<String>` on each task. Enables sub-agent delegation and
  prevents duplicate work in multi-session scenarios.
- Effort:  **▲**

### Cross-session task synchronisation
- **speculative**
- A protocol (file-watch, MCP broadcast, or shared KV) for multiple agent
  sessions to see the same live task list. Claude Code does this via
  `CLAUDE_CODE_TASK_LIST_ID` environment variable and file-polling.
- Prerequisite: persistence and owner fields.
- Effort:  **▲▲▲**

---

## Performance & Correctness

### Indexed fields for O(1) lookup
- **potential**
- The current `find(|i| i.id == id)` is O(n) per action. For lists up to
  ~100 items this is fine. A `HashMap<String, usize>` index inside `TodoList`
  would be O(1). Pre-compute on clone or use `im` (immutable data structures).
- Not benchmarked for agent CLIs specifically, but standard CS optimisation.
- Effort:  **▲▲**

### ID collision hardening
- **potential**
- `next_id` scans `task-{N}` suffixes and increments. If a user (LLM) manually
  creates a task with ID `task-999999`, the next auto-ID will be
  `task-1000000`. Consider prefix-based scoping or UUID suffixes.
- Effort:  **▲**

---

## Summary: Top 5 by Speculative Value

| Rank | Idea | Category | Effort |
|---|---|---|---|
| 1 | `NextTask` action (pick best work item) | Prioritisation | ▲ |
| 2 | `Blocked` status + auto-transition | State | ▲▲ |
| 3 | Sub-tasks / hierarchical tasks | Hierarchy | ▲▲▲ |
| 4 | List with filters | Actions | ▲ |
| 5 | GetTask (view single item) | Actions | ▲ |

## Summary: Top 5 by Potential (Benchmark Evidence)

| Rank | Idea | Category | Evidence Source | Effort |
|---|---|---|---|---|
| 1 | Dependency cycle detection | Deps | Standard correctness (no deadlock) | ▲ |
| 2 | Auto-cascade stale deps on delete | Deps | Docker Agent (prod) | ▲ |
| 3 | Step efficiency tracking | LLM Guidance | TDAG, SWE-agent papers | ▲▲ |
| 4 | Indexed field lookups | Performance | Standard CS | ▲▲ |
| 5 | ID collision hardening | Correctness | Covers edge case | ▲ |
