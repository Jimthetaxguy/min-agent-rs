# CONTEXT.md - Ubiquitous Language Glossary

**min-agent** is a small, read-only, protocol-oriented Rust coding agent: one library, one thin
CLI, one bounded tool loop, and three wire adapters (OpenAI Chat, OpenAI Responses, Anthropic
Messages). It reads a workspace and answers questions; it never edits or executes. See
`README.md` for use, `docs/verification.md` for what was checked, `docs/compatibility.md` for
live-model results, and `docs/provenance.md` for reuse decisions.

## Domain Vocabulary

- **Connection**: Where a request goes: `protocol`, base URL, `auth`, optional explicit `proxy`
  (`config.rs`). Its **fingerprint** is a stable FNV-1a hash of protocol, endpoint, auth
  description, and proxy presence; it contains no secret.
- **Model Profile**: Which model and features are used on a connection (model ID, native tools,
  output-token limit). A user picks a named profile; the agent never guesses.
- **Protocol**: `openai_chat`, `openai_responses`, or `anthropic_messages`. Each has an
  **adapter** in `adapters/` that renders requests and validates responses.
- **Auth**: `None`, `BearerEnv { env }`, or `HeaderEnv { header, env }`. Credentials come only
  from the named environment variable.
- **Item**: One neutral conversation entry: `User`, `Assistant(native)`, or `ToolResult`.
  `Assistant` holds the provider's native output verbatim, so opaque continuation (reasoning,
  signatures) replays unchanged within one run and one client.
- **ModelClient / ModelTurn**: The seam between the loop and the wire. A turn is visible text,
  native tool calls, the native output, and optional **Usage** (unknown is `None`, never zero).
- **ModelError**: `RequestTooLarge`, `Provider(ProviderFailure)`, or `Invalid(reason)`.
  `ProviderFailure::retryable()` decides bounded retry.
- **Budget**: Every run limit in one struct: rounds, calls, run deadline, request and tool
  timeouts, retries, repeat limit, and request/response/tool-output byte caps.
- **StopReason**: The typed end of a run: `Completed`, `BudgetExceeded { limit }`,
  `ProviderError { failure, attempts }`, `InvalidResponse { reason }`, `TraceFailed`. Only
  `Completed` is success.
- **RunReport**: Returned by every run: stop reason, answer (only if completed), last text,
  counts, usage, per-call **CallRecord**s (metadata only), and the transcript.
- **Protocol violation vs environment answer**: Malformed arguments, duplicate IDs, unknown
  tool, truncation, or refusal stop the run as `InvalidResponse` with nothing executed. Not
  found, denied, wrong type, or invalid arguments are returned to the model as tool errors.
- **Error streak / repeated batch**: The two loop breakers: N consecutive failures of the same
  tool and error kind, or N identical consecutive batches (argument key order ignored).
- **Workspace / Prepared / Effect**: `tools.rs`. `Workspace` is a `cap-std` directory
  capability. `prepare()` validates arguments and path policy before any read and returns an
  opaque `Prepared`; `execute()` performs the bounded read. `Effect` has one variant, `Read`;
  adding a variant is gated on the entry criteria in the README.
- **Redaction**: Known credential shapes in tool output become `[REDACTED]` (`redact.rs`).
  A backstop, not a scanner.
- **Trace**: Optional append-only JSONL run record `{v, seq, wall_time, run_id, kind, payload}`;
  metadata only; readers skip unknown kinds.
- **Text-only mode**: No tool schema is sent and no tool call can execute.
- **doctor**: Offline config and credential-presence check; no model request.
- **Graduation test**: `tests/live.rs`, opt-in, synthetic workspace with a planted fact; each
  run adds a row to `docs/compatibility.md`.

## Current state (2026-09-18)

0.2.0: the read-only agent is feature-complete for this design stage. Three adapters with
shared conformance fixtures and real HTTP round trips; typed stops; recoverable tool errors;
bounded retry; JSONL trace; deterministic, UTF-8-safe, fit-to-cap tools; redaction; hardened
CI with MSRV 1.88 and cargo-deny. Live runs so far: local Ollama only (see
`docs/compatibility.md`). Hosted-provider graduation needs explicit authorization.

Deferred by design: edit/exec (gated on effect typing, attempt fencing, digest-bound
approval, and a process supervisor), sessions, streaming, compaction, MCP, TUI, and
in-run provider switching.
