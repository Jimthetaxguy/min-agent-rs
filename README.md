# min-agent

A small read-only Rust coding agent: one library, one CLI, one bounded loop, and three
wire-protocol adapters (OpenAI Chat Completions, OpenAI Responses, Anthropic Messages).
It reads a workspace and answers questions about it. It never edits files or runs commands.

## Build and use

```sh
cargo build
cargo run -- --help
cargo run -- --config examples/connections.toml doctor --profile local
cargo run -- --config examples/connections.toml ask --profile local \
  --workspace /path/to/project "Explain the structure of this project."
cargo run -- --config examples/connections.toml ask --profile local \
  --text-only "Explain Rust ownership."
```

Replace the placeholder model IDs first. Credentials are read only from the environment
variable named in your explicit config, never from another agent's credential store. Set
a key in your own terminal or secret manager; do not put keys in the TOML file. `doctor`
is offline and makes no model requests.

`ask` options:

| Flag | Effect |
| --- | --- |
| `--trace FILE` | Append a JSONL run trace (metadata only) to `FILE` |
| `--report-json` | Print the full run report as JSON on stderr |
| `--max-rounds`, `--max-calls` | Override the round and tool-call budgets |
| `--timeout-secs`, `--request-timeout-secs` | Override the run and per-request deadlines |
| `--max-retries` | Retries of transient provider failures per round |

Exit codes: `0` completed, `1` configuration or usage error, `2` budget exceeded,
`3` provider error, `4` invalid model response, `5` trace could not be written. Only
exit `0` prints an answer on stdout; other stops print the reason (and any partial model
text, labeled as not an answer) on stderr.

## Protocols and configuration

A **connection** says where requests go (`protocol`, `base_url`, `auth`, optional
`proxy`); a **model profile** says which model and features to use on it.

| `protocol` | Endpoint | Notes |
| --- | --- | --- |
| `openai_chat` | `{base_url}/chat/completions` | OpenRouter, LiteLLM, Ollama, and other compatible servers |
| `openai_responses` | `{base_url}/responses` | Stateless: `store:false`; encrypted reasoning items are requested and replayed in order |
| `anthropic_messages` | `{base_url}/messages` | Sends `anthropic-version: 2023-06-01`; profiles must set `max_output_tokens` |

`auth` is `{ kind = "none" }`, `{ kind = "bearer_env", env = "..." }`, or
`{ kind = "header_env", header = "x-api-key", env = "..." }`. `proxy` is an explicit
`http://` or `https://` URL without credentials, allowed only for HTTPS endpoints; ambient
`HTTP(S)_PROXY` variables are never used. Endpoints must be HTTPS or numeric-loopback HTTP.

The native assistant output of each turn (Chat message, Responses output items, Messages
content blocks, including opaque reasoning) is replayed verbatim within the same run and
client only. Nothing is replayed across connections or runs.

See `docs/compatibility.md` for what has actually been run against real models.

## Run semantics

Every run returns a `RunReport` with a typed `StopReason`:

| Stop reason | Meaning |
| --- | --- |
| `Completed` | The model gave a final answer |
| `BudgetExceeded { limit }` | `rounds`, `tool_calls`, `wall_clock`, `context_bytes`, `repeated_batch`, or `repeated_tool_error` |
| `ProviderError { failure, attempts }` | Transport, timeout, HTTP status, oversize or non-JSON body, or provider error envelope |
| `InvalidResponse { reason }` | The model broke the protocol: malformed or non-object arguments, duplicate or reused call IDs, unknown tool, truncated or refused output, or tool calls in text-only mode. Nothing in that response runs |
| `TraceFailed` | A requested trace could not be written, so the run stopped rather than continue unaudited |

A budget stop is never reported as success. Tool errors caused by the environment
(file not found, path denied, wrong file type, invalid arguments) are **returned to the
model as tool results** so it can correct itself. Three consecutive failures of the same
tool with the same error kind stop the run, as do three identical consecutive batches.
Argument key order does not affect the repeated-batch check.

Transient provider failures (timeouts, connection errors, 408/429/5xx/529) are retried
up to `max_retries` times per round within the run deadline. A `Retry-After` delay is
honored in full; if it does not fit in the remaining run time, the run stops instead.
Model requests are effect-free here; tools are never retried. A request whose timeout was
shortened by the run deadline stops as `wall_clock`, not as a provider error. Token usage
becomes unknown (`null`) after any failed attempt that may have generated output.

Default budget (`agent::Budget`, all overridable by library callers): 10 rounds, 40 tool
calls, 180 s run deadline, 90 s per model request, 20 s per tool call, 2 retries,
repeat limit 3, 256 KiB request, 1 MiB response, 32 KiB tool result. There are no other
run limits. Tool-level scan limits are listed below and are part of each tool's contract.

## Tools

| Tool | Behavior |
| --- | --- |
| `list_files` | Immediate entries of one directory, sorted by name; examines at most 5,000 entries |
| `read_file` | Up to 8,192 bytes from a byte offset; pages end on UTF-8 character boundaries; paginate with `next_offset` |
| `search_text` | Literal search over redacted text, depth-first in name order: at most 20,000 entries, depth 8, 256 KiB per file, 16 MiB total; binary, unreadable, and hardlinked files are counted in `skipped_files` |

Every result reports truncation. Output that would exceed the tool-result cap is
shortened to fit (a read page ends earlier; lists and searches return fewer items)
rather than failing. Paths in results are workspace-relative (`src/main.rs`, never
`./src/main.rs`).

## Filesystem and privacy boundaries

Tool paths are relative to a selected workspace. Traversal, absolute paths, symlink
paths, common credential filenames (`.env*`, `*.pem`, `*.key`, `*.tfstate`, `id_*` keys,
`.netrc`, `.npmrc`, `.pgpass`, `.git-credentials`, `.kube`, `.docker`, `credentials.json`, ...),
`.git`, `target`, and `node_modules` are denied or skipped. FIFOs, sockets, and devices
are refused before open. Hardlinked files are listed but their contents are refused.
Opens are capability-relative via `cap-std`, not host-path opens after a canonicalization
check. This is a read-only filesystem boundary, not a process or network sandbox.

File contents returned by tools are sent to the selected model endpoint; the CLI prints
that destination before asking. Well-known credential shapes in content (AWS access key
IDs, GitHub tokens, `sk-` keys, Slack tokens, Google API keys, PEM private-key blocks)
are replaced with `[REDACTED]`. Detection runs over context around each read page, so a
token cut by a page boundary or a chosen offset, or a key body far from its BEGIN line, is
still masked (context reaches 8 KiB back, which covers common RSA and EC keys);
`search_text` matches against redacted text, so it cannot be used to probe a credential
character by character. This is a backstop, not a secret scanner: it does not catch secrets
in other formats. Use a synthetic or sanitized workspace for first live tests.

**Prompt injection.** Workspace files are untrusted input. A file can contain text that
tries to steer the model. The system prompt labels tool output as data, and the tool set
limits what an injected instruction can achieve: it can make the model read or search
more of the workspace (and so send more of it to the endpoint), or distort the answer.
It cannot write, execute, or reach any network destination other than the configured
endpoint.

Concurrent hostile mutation of the workspace is not a supported threat model:
capability-relative opens guard root escape, but name and file-type checks happen before
the open and are not an atomic content-classification boundary. Use a stable workspace.

## Run records

With `--trace FILE`, each run appends JSON lines of the form
`{"v":1,"seq":N,"wall_time":<unix ms>,"run_id":"...","kind":"...","payload":{...}}`.
Kinds: `run_start` (profile, protocol, endpoint, model, connection fingerprint, policy
version, tool list, effects, effective budget, workspace), `model_response`,
`model_error`, `tool_call` (call ordinal, tool, path, ok or error kind, bytes, truncated, redacted),
and `run_end` (stop reason, counts, token usage). Unknown usage is recorded as `null`,
not zero. Readers must skip kinds they do not recognize. Traces contain no file content,
prompts, model text, or other model-controlled strings (provider call IDs and unknown tool
names are not recorded). If any trace write fails, the run stops before its next tool call
and ends as `TraceFailed`, including when only the final `run_end` line fails.

## Not applicable by construction

Several questions that matter for agents with side effects have no subject here, because
the only effect class is `Read` (`tools::Effect` has one variant and a test enforces it):
there are no resource versions to conflict on, no child processes to supervise or cancel,
no side effects that could be uncertain after a crash, no commits that a late attempt
could race, and no delegation. Adding any effectful tool first requires, as entry
criteria: effect typing separate from capability, an effect-state record with recovery
rules, attempt fencing, approval bound to a digest of the exact action and target version,
and a process supervisor with descendant cleanup.

## Verify

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked
python3 tests/acceptance.py target/debug/min-agent
cargo deny check
```

CI runs these on Linux and macOS, plus an MSRV (1.88) check. The workflow uses a
read-only token, no secrets, and actions pinned by commit SHA, because fork pull requests
execute build scripts from the dependency graph.

The live-provider test is opt-in and sends real requests:

```sh
MIN_AGENT_LIVE_CONFIG=connections.toml MIN_AGENT_LIVE_PROFILE=openrouter \
  cargo test --test live -- --ignored --nocapture
```

See `docs/verification.md` for the executed verification record.

## Not included

Editing, shell execution, persistent sessions, streaming, automatic compaction, MCP, TUI,
OAuth, provider switching during a run, and usage pricing. Ctrl+C terminates the process;
there are no child processes or effects to recover.
