# Cosh Telemetry

## In one sentence

Cosh telemetry exists to measure product health and aggregated usage. It
**does not read or send conversations**: prompts, model responses, messages,
file contents, and tool inputs and outputs are not part of the events sent.

The current client behavior is described below: what is stored, why it is
stored, and where the system's boundaries are.

## What is never collected

The client does not send:

- prompts, responses, history, or any chat text;
- file contents, local paths, directories, code, or diffs;
- tool inputs, outputs, or arguments;
- API keys, tokens, cookies, private URLs, hosts, or IP addresses;
- usernames, email addresses, hostnames, MAC addresses, or system identifiers;
- literal model names; provider and tool names outside the allowlists are also not sent literally.

Token counts mean only the number of tokens, never the tokens or the text they
represent. The reported cost is an aggregated total in cents, not an invoice
or request content.

## The exception that must be known

The uninstall command optionally asks why the user is uninstalling. If the
user types an answer, the client may send that text in the `uninstall` event,
limited to 256 characters and subject to the safety filter. Pressing Enter
does not send an answer.

This is the only free-text field typed by the user in the current schema, and
it is not chat content. It is still user-provided text. Therefore, the
accurate statement is: **Cosh does not collect chat data; apart from optional
uninstall feedback, telemetry does not send free-form user text**.

## Why this data exists

| Aggregated data | Purpose |
|---|---|
| Duration, turns, and messages | Understand adoption and session duration. |
| Provider and model usage | Prioritize compatibility and support. |
| Tools and screens used | Identify areas that need maintenance or improvement. |
| Input/output token counts and aggregated cost | Measure consumption and detect efficiency regressions. |
| Compactions and configured MCP server count | Evaluate context pressure and integration usage. |
| Error and failure categories | Measure stability without storing the original message. |
| Version, operating system, and architecture | Compare issues across versions and platforms. |
| Update and uninstall results | Measure adoption, removal, and distribution health when those events are emitted. |

These are product and engineering metrics. They are not used to reconstruct a
conversation or create a personal profile.

## What is generated today

In the active application path, the TUI generates one aggregated summary when
a session ends. The uninstall flow generates a separate event. The code also
defines formats for `install`, `error`, `crash`, and `update`, but those types
are part of the schema and do not currently have an active emission point in
the TUI flow.

### `session_summary`

This is one event per session, created when the TUI exits. It contains only:

- duration, rounded to 30-second blocks;
- turn counts and newly created transcript message counts (not streamed chunks
  or history restored from disk);
- counts by allowed provider;
- counts by model, using a hash for unknown names;
- counts by allowed feature and tool;
- the number of configured MCP servers, without their names;
- the number of committed context-compaction checkpoints (not attempts or
  duplicate lifecycle notifications);
- input and output token counts;
- aggregated cost in cents;
- error categories, error fingerprints, allowed internal source, and occurrence counts;
- whether the update banner was shown.

An error never carries its original message. The message is normalized and
turned into a fingerprint to group similar failures; the text is discarded
before the event is saved.

### `uninstall`

The uninstaller emits this event when telemetry is enabled. It contains:

- the random installation identifier;
- operating system and architecture;
- whether optional feedback was provided;
- the feedback, when it passes the filter and is not omitted;
- whether the binary, data/config/cache directories, and credentials were removed.

Feedback is trimmed, limited to 256 characters, and rejected when it contains
patterns such as local paths, URLs, keys, tokens, or private addresses. The
filter reduces risk, but does not turn this field into a statistic; that is
why it is documented as an exception.

## Common event metadata

Every valid event has a small set of technical metadata:

| Field | Content | Protection |
|---|---|---|
| `schema_version` | Format version | Validated by the client and at send time |
| `app_version` | Cosh version | Numeric version shape only |
| `install_id` | Persistent random UUIDv4 per installation | Not derived from a user, hostname, MAC address, or network address |
| `client_event_id` | Random UUIDv4 for the event | Deduplication, not identity |
| `event_type` | Closed event type | Known enum |
| `occurred_at` | UTC date/time rounded to the minute | No second-level precision |
| `session_id` | Session identifier when an event uses one | Optional; the current summary does not populate it |

`install_id` lets the service group metrics from the same installation. The
client does not associate it with an account, person, machine, or network.

## How names are handled

The schema uses closed sets and cardinality limits:

- known providers remain only as names from an allowlist; other names become a
  16-character hash;
- built-in tool dispatch names map to allowed names or categories, such as
  `bash_run` → `bash` and `skills_read` → `skills`; unknown tools become `other`;
- features are enums such as `session`, `settings`, `tools`, and `rag`;
- model names are hashed, so the literal identifier is not sent;
- error sources must belong to the internal allowlist;
- the number of aggregate keys is limited to prevent unbounded growth or
  cardinality-based leakage.

Before entering the queue, and again before sending, every event is validated.
If a value cannot be validated, the event is discarded. This is **fail
closed**: losing a metric is preferable to transmitting an unexpected value.

## Where data is stored

While waiting to be sent, events are kept locally in a JSONL queue inside
Cosh's data directory:

```text
<Cosh data directory>/telemetry/telemetry-events.jsonl
```

On Unix, this normally corresponds to
`~/.local/share/cosh/telemetry/telemetry-events.jsonl`. The directory is
created with owner-only permissions when the platform provides that control.

The queue is limited to 512 KiB and 2,000 events. An individual event larger
than 64 KiB is rejected. Events older than 14 days are removed when a send is
attempted. When the queue is full, the oldest events are discarded. These
limits protect local disk space and do not represent a server-side retention
promise.

The persistent installation identifier is stored in the same directory, in
`install_id`, and is a UUIDv4 created with operating-system entropy. If
entropy or storage fails, the event is discarded; there is no time-, address-,
or hardware-derived fallback.

## How sending works

The client only queues events when telemetry is enabled. The session summary
is queued when the TUI exits; sending is attempted after the terminal has been
restored.

The transport:

- uses HTTPS only;
- does not follow redirects;
- does not use the machine's automatic proxy;
- revalidates every persisted event before transmitting it;
- sends at most 50 events per batch;
- retries transient network failures, HTTP 429, and HTTP 5xx responses;
- limits each flush to a total network budget of three seconds, including
  requests and retry waits; a timeout returns the batch to the local queue;
- keeps the batch in the queue when a transient failure prevents delivery;
- discards a permanent 4xx response because retrying would not fix it.

Without valid ingest configuration, nothing leaves the machine: events remain
in the local queue until they expire or are removed by the normal queue
limits. The destination is an HTTPS ingest endpoint configured by the Cosh
distribution; the client uses a publishable key, not an administrative
secret.

This repository describes and tests client behavior. Retention, internal
access, and deletion policy in the ingest service are not defined in this
code, so the project must publish the server-side retention policy separately.

## Consent and disabling telemetry

Telemetry is enabled by default, but it is opt-out. The effective decision
follows this order:

1. `CI` permanently disables telemetry for that process;
2. `COSH_TELEMETRY=off`, `0`, or `false` disables it;
3. `COSH_TELEMETRY=on`, `1`, or `true` enables it;
4. without an override, the persisted user preference applies.

The TUI exposes this preference at `/settings` → **General** → **Telemetry**.
When disabled, the application records no new events and does not send an
old queue. The complete configuration reference is in
[docs/usage/setup.md](../usage/setup.md).

## Trust summary

- Chat is not telemetry.
- Prompt and response text is not telemetry.
- File and tool content is not telemetry.
- Most of the system is made of counts, enums, sums, versions, or hashes.
- The installation identifier is random and is not personal identity.
- The only current free-text field is optional uninstall feedback, explicitly
  documented above.
- The implementation validates and rejects data outside the schema before
  storing or sending it.
