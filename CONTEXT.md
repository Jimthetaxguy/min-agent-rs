# CONTEXT.md - Ubiquitous Language Glossary

**min-agent** is a small, read-only, protocol-oriented Rust coding agent: one library, one thin
CLI, one bounded tool loop, and one OpenAI Chat Completions adapter. It is the first verified
slice of the fx-inspired design in `_working-files/minimal-rust-agent-design.md` — a walking
skeleton, not a finished replacement for fx, Codex, or Grok Build. See `README.md` for build/use
and `docs/verification.md` / `docs/provenance.md` for what has actually been checked and reused.

## Domain Vocabulary

- **Connection**: Where a request goes — protocol, base URL, and auth method (`config.rs`).
  Independent from **Model Profile**, which is which model/features are permitted on top of a
  connection. A user picks a named profile (e.g. `local`, `openrouter`); the agent never guesses.
- **Auth**: `None` or `BearerEnv { env }` — credentials are read only from the exact named
  environment variable in the config, never inferred or borrowed from another tool's store.
- **ModelClient**: The seam between the bounded loop (`agent.rs`) and the wire protocol
  (`model.rs`). `ChatClient` is the only implementation today (OpenAI Chat Completions); Responses
  and Anthropic Messages adapters are designed but not yet built.
- **ModelTurn / ToolCall**: The normalized result of one model round — visible text plus zero or
  more native tool calls, with the original assistant message preserved verbatim so opaque
  extensions (e.g. `reasoning_content`) replay correctly within the same run.
- **Budget**: Per-run limits — model rounds, total tool calls, wall-clock deadline, and a
  repeated-tool-batch breaker (`agent.rs::Budget`). A run always ends with a typed stop reason,
  never a silent success on a budget cut.
- **Text-only mode**: No tool schema is sent and no tool call can execute — set explicitly via
  `--text-only` or a model profile's `native_tools = false`.
- **Workspace / Prepared**: `tools.rs`. `Workspace` opens a directory as a `cap-std` capability
  (beneath-root resolution enforced at open, not just canonicalize-then-open). `prepare()`
  validates a tool call's arguments and path policy *before* any content is read; `execute()` then
  performs the bounded read. The three tools are `list_files`, `read_file`, `search_text` — all
  read-only.
- **Typed stop reasons**: Errors are prefixed so callers can distinguish `BudgetExceeded`,
  `InvalidResponse` (malformed/refused/incomplete model output), and `ProviderError` (transport or
  HTTP-status failure) from each other and from ordinary Rust errors.
- **doctor**: An offline CLI subcommand that resolves configuration and checks credential presence
  without making any model request.

## Current state

Landed and verified on this machine 2026-09-18: `cargo fmt`, `cargo build --locked`,
`cargo test --locked` (11 unit + 1 HTTP integration test), `cargo clippy --all-targets -D
warnings`, and the Python black-box acceptance driver (11/11) all pass. Not yet decided: whether
this gets a GitHub remote (see `docs/verification.md` and session notes — deliberately left as an
open question rather than assumed).

Deferred by design (see `README.md` "Not included yet"): Responses/Anthropic Messages adapters,
persistent sessions, file edits, shell execution, streaming, MCP, TUI, provider switching within a
run, and any live-provider testing.
