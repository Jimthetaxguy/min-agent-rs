# min-agent

A small read-only Rust coding agent: one library, one CLI, one bounded loop,
and one OpenAI Chat Completions protocol adapter. This is the first slice of
the fx-inspired design, not a finished replacement for fx, Codex, or Grok Build.

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

Replace the placeholder model ID first. Credentials are read only from the
environment variable named in your explicit config, never from another agent's
credential store. Set a key in your own terminal or secret manager; do not put
keys in the TOML file. `doctor` is offline and makes no model requests.

The same adapter is intended for configured OpenRouter, LiteLLM, and compatible
local endpoints. Compatibility is a target, not a claim of live testing:
validation uses loopback fake HTTP servers only. Endpoints must support the
native Chat Completions function-tool contract for tool-using runs.

## Included

- Public `ModelClient` seam and `run` function; no CLI dependency in the core.
- Explicit connection/profile configuration, native assistant replay (including
  opaque reasoning extensions) inside one run, and no automatic fallback.
- `list_files`, `read_file`, and literal `search_text` workspace tools.
- Native tool calls only. Complete batches are validated before reading contents.
- 10 model rounds, 40 calls, 180-second run deadline, repeated-batch breaker,
  256 KiB context/request cap, 1 MiB response cap, 32 KiB tool-result cap.
- Numeric loopback HTTP or remote HTTPS, redirects disabled, ambient HTTP proxies
  disabled, bounded transport reads, and no credentials in diagnostics.
- Text-only mode: no tool schemas or execution. `native_tools=false` in a model
  profile also selects this behavior.

## Filesystem and privacy boundaries

Tool paths are relative to a selected workspace. Traversal, symlink paths, common
credential filenames, `.git`, `target`, and `node_modules` are denied or skipped.
Regular-file checks and Unix hardlink rejection apply to file contents.
Capability-relative opens use `cap-std` rather than host-path open after a
canonicalization check. This is a read-only filesystem boundary, not a process
or network sandbox.

File contents returned by tools are sent to the selected model endpoint. The
program prints that destination before asking. Filename exclusions do not detect
every secret inside ordinary source files. Choose a synthetic or sanitized
workspace for first live tests.

Concurrent hostile mutations are not a supported threat model for this initial
release: capability-relative opens guard root escape, but secret-name checks and
file-type checks are not an atomic content-classification boundary. In particular,
special-file replacement races can interfere with blocking I/O. Use a stable,
trusted workspace snapshot, not a directory writable by an adversary.

`read_file` pagination uses byte offsets; split UTF-8 sequences may display
replacement characters. Search skips non-UTF-8/oversized files and bounds depth
and entries. Tool errors stop the run instead of retrying or silently falling back.

## Verify

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --locked
python3 tests/acceptance.py target/debug/min-agent
```

Rust tests include actual HTTP tool-result exchange with a local fake model,
native argument validation, workspace path policy, and loop stopping.
See `docs/verification.md` for the executed verification record.

## Not included yet

Native Responses and Anthropic Messages adapters, persistent sessions, editing,
shell execution, streaming, automatic compaction, MCP, TUI, OAuth, provider
switching during a run, usage pricing, and live-provider verification are deferred.
The library's budget is configurable by callers; this initial CLI uses its fixed
conservative defaults. Ctrl+C terminates the process; there are no child processes
or effects to recover.
