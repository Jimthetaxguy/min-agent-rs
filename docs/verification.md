# min-agent verification

Verified on September 18, 2026. This record covers the read-only first slice,
not all capabilities in the design plan.

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

## Local placement

Verified source:

```text
/tmp/min-agent-verified-871e418e/min-agent-rs
```

Mac executable:

```text
/tmp/min-agent-verified-871e418e/target/debug/min-agent
```

Intended durable destination:

```text
/Users/jamespustorino/code/min-agent-rs
```

The durable destination could not be created because the app reported no active
writable workspace. Textual authorization was received, but it did not change
the tool's filesystem grant. Nothing was written into existing repositories.
The temporary directory is not a durable installation; preserve the source
archive or move it into an approved workspace before relying on it.

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
IMPULSE, ROSA, Codex, or Grok Build. See provenance.md for the reuse decisions.
