# min-agent verification

## Read-only agent, complete (0.2.0)

Verified on September 18, 2026 on Apple Silicon macOS, Rust 1.98.1, from the repository root.

| Check | Result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked` | 44 unit tests, 9 HTTP integration tests passed; 1 live test ignored (opt-in) |
| `cargo build --locked` | Passed |
| `python3 tests/acceptance.py <binary>` | 25 checks passed |
| `cargo deny check` | advisories, bans, licenses, sources ok |
| `cargo +1.88.0 check --locked --all-targets` | Passed (MSRV) |
| `tests/live.rs` against local Ollama | See `compatibility.md`: 1 pass, 1 recorded model failure |

What the added tests cover:

- **Protocol conformance.** One shared list of 15 named cases (single and multiple calls,
  final text, opaque reasoning preserved, refusal, truncation, empty final, missing call ID,
  malformed and array arguments, duplicate IDs, error envelope, stop-reason mismatch,
  unknown item, usage) that each of the three adapters must supply and pass. Request
  serialization fixtures per adapter.
- **Real HTTP round trips** for Chat, Responses, and Messages (the last with header auth and
  grouped tool results), through the full loop.
- **Hostile provider:** chunked oversize body without Content-Length, slow body against the
  request deadline, cross-origin redirect (not followed; the target receives nothing),
  event-stream reply to a non-streaming request, 503 retried then succeeding, context budget
  stopping before any request is sent.
- **Hostile workspace:** FIFO refused without blocking, 8 KiB of NUL bytes shrunk to fit the
  result cap, credential redaction, symlink and hardlink refusal, path traversal and secret
  paths returned as tool errors with no leak.
- **Loop semantics:** typed stop reasons, recovery after a tool error, same-kind error
  streak, unknown tool as a protocol violation with nothing executed, bounded retry,
  partial text kept on a stop, key-order-insensitive repeat detection (pins serde_json
  without `preserve_order`), usage unknown rather than zero, trace envelope and content-free
  trace.

An independent review of this change found and the same change fixed: a proxy on a
loopback-HTTP endpoint (now refused), redaction bypass by offset, page split, or search
probing (now context-window redaction and redacted search), missing credential filenames,
usage undercount after failed attempts, a deadline-capped timeout reported as a provider
error, silently skipped search files, and a hardlink search test that passed vacuously.

The acceptance driver previously checked for an `Authorization` header with a
case-sensitive lookup against lowercase header names, so its "no credential sent" check
passed vacuously. Headers are now normalized, and the check is real.

## Read-only walking skeleton (0.1.0)

This earlier record covers the first read-only slice.

## Isolated Linux build

Rust 1.93.1; dependencies pinned in Cargo.lock.

| Check | Result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo test --locked` | 11 unit tests and 1 HTTP integration test passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo build --locked` | Passed |
| `python3 tests/acceptance.py target/debug/min-agent` | 11 checks passed |

The acceptance driver launches a loopback fake model and temporary synthetic
workspace. It verifies native tool-call/result linking, opaque assistant field
replay, text-only requests, malformed/array arguments, duplicate IDs, truncated
tool calls, unknown tools, path traversal, secret-path and symlink rejection,
and stopping repeated tool batches with distinct IDs.

Unit tests additionally cover read pagination, search line numbering, exclusion
behavior, list scan truncation, hardlink rejection, deadlines, total-call and
prompt bounds, URL policy, and missing-credential behavior.

## Apple Silicon Mac build

Rust 1.98.1. Built offline with the exact locked dependencies vendored separately
for the verification run. The source archive does not include these dependencies.

| Check | Result |
| --- | --- |
| Compilation of binary and tests | Passed |
| Unit tests | 11 passed |
| `cargo fmt --check` | Passed |
| Clippy, all targets, warnings denied | Passed |
| CLI `--help` | Passed |
| Offline `doctor --profile local` | Passed |
| HTTP integration test | Blocked: loopback socket bind returned `PermissionDenied` |
| Python HTTP acceptance driver | Not run on Mac because loopback sockets are blocked |

The Mac's local-command sandbox prevents network access, including the test
server's loopback bind. The HTTP integration test is not marked passed on Mac;
it passed on Linux. A complete `cargo test` therefore exits nonzero in this
particular Mac command sandbox.

## Second Apple Silicon Mac build (repo root)

Rust 1.98.1, same machine architecture as above. Run from the repository root
after landing, rather than a temporary directory.

| Check | Result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo build --locked` | Passed |
| `cargo test --locked` | 11 unit tests and 1 HTTP integration test passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| CLI `--help` | Passed |
| Offline `doctor --profile local` | Passed |
| `python3 tests/acceptance.py` | 11 checks passed |

Unlike the first Mac run above, the local-command sandbox here permitted a
loopback bind, so the HTTP integration test and the Python acceptance driver
both ran and passed. Loopback-bind restrictions are therefore a property of
the particular sandbox in use, not of Apple Silicon Macs generally.

## Limits of the verification

- No real provider requests, user credentials, or paid inference were used.
- OpenRouter, LiteLLM, and local-server configurations are examples, not live
  compatibility certifications.
- Only the OpenAI Chat Completions wire protocol is implemented. Native Responses
  and Anthropic Messages remain planned adapters.
- No edits, shell execution, persistent sessions, MCP, streaming, or TUI.
- No claim of resilience to an adversary concurrently mutating the workspace.
- Filename exclusions are not a general secret scanner.
- No release-binary benchmark or dependency vulnerability audit was performed.

The package is an independently implemented small agent, not vendored code from
any other project. See provenance.md for the reuse decisions.
