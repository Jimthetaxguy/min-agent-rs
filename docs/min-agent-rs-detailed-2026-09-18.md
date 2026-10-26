# min-agent-rs — detailed architecture, state, insights, use cases, and Python appendix

**Date:** 2026-09-18 (evening ET)  
**Repo:** https://github.com/Jimthetaxguy/min-agent-rs  
**Local path:** `/Users/jamespustorino/code/min-agent-rs`  
**Audience:** Harness Handlers + James  
**Baseline commit discussed as verified:** `9baa811` (`main` / `origin/main`)  
**Working tree:** dirty WIP on `feat/complete-read-only-agent` (not stamped)

---

## 1. Purpose

`min-agent` is a small **read-only** coding agent in Rust. Design intent: one library, one thin CLI, one bounded tool loop, and protocol adapters that talk to Chat Completions–compatible endpoints (and, in WIP, Responses / Anthropic Messages).

It is the first verified slice of an fx-inspired design — a walking skeleton for:

- host-owned authority over filesystem reads
- explicit budgets and stop conditions
- a clean model↔tools seam
- offline diagnostics (`doctor`)

It is **not** a replacement for Codex, Grok Build, or fx, and it is **not** (yet) an editable agent or a Monty/code-mode runtime.

---

## 2. Architecture

### 2.1 Layer diagram

```
┌─────────────────────────────────────────────────────────────┐
│ CLI (main.rs)                                               │
│  parse args → load Config → open Workspace → call agent::run│
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│ Agent loop (agent.rs)                                       │
│  Budget / StopReason / RunReport                            │
│  system prompt (read-only policy)                           │
│  drive: model turn → validate tool batch → prepare/execute  │
└───────────────┬─────────────────────────────┬───────────────┘
                │                             │
┌───────────────▼──────────────┐ ┌────────────▼────────────────┐
│ Model seam (model.rs)        │ │ Tools (tools.rs)            │
│  ModelClient trait           │ │  Workspace (cap-std)        │
│  HttpModelClient             │ │  prepare → Prepared         │
│  Item / ModelTurn / Usage    │ │  execute → ToolOutput       │
│  RequestLimits               │ │  list / read / search       │
└───────────────┬──────────────┘ └─────────────────────────────┘
                │
┌───────────────▼──────────────┐
│ Protocol adapters            │
│  chat (Chat Completions)     │  ← landed in verified slice
│  responses / messages (WIP)  │  ← untracked / dirty tree
└──────────────────────────────┘
```

### 2.2 Core types and responsibilities

| Module | Responsibility |
|---|---|
| `config` | Connections, auth (`BearerEnv`), model profiles, protocol enum, URL/proxy validation, FNV helpers |
| `model` | Provider-independent turn model: `ToolCall`, `ToolSpec`, `Item`, `Usage`, `ModelTurn`, `ModelClient`, `HttpModelClient`, `ModelError` / `ProviderFailure` |
| `agent` | Bounded loop: `Budget`, `Limit`, `StopReason`, `RunReport`, `RunOptions`, `run` / `drive`, policy version + system prompt |
| `tools` | Capability-relative `Workspace`; `Prepared` minting; `ToolError` / `ToolErrorKind`; scan/read/search limits; tool definitions |
| `adapters` | Wire encode/decode per protocol (`chat`, `responses`, `messages`) |
| `redact` / `trace` | WIP: secret redaction helpers; structured run trace (`TRACE_VERSION`) |
| `main` | CLI surface: ask / doctor, budget args, exit codes from `StopReason` |

### 2.3 Control loop (intended contract)

1. Build system prompt stating read-only policy (`POLICY_VERSION`, e.g. `read-only/2` in WIP).
2. Call `ModelClient` with conversation items + tool specs (unless text-only).
3. Parse a `ModelTurn` (text + zero or more native tool calls). Preserve opaque assistant fields for replay within the same run.
4. **Prevalidate the entire tool batch** before any workspace read (IDs, tool names, argument shape).
5. For each call: `prepare` (path policy + arg bounds) then `execute` (bounded I/O).
6. Append tool results; repeat until final answer or a stop condition.
7. Return a `RunReport` with outcome, counts, usage, and stop reason.

### 2.4 Authority and safety model

- **Host owns authority.** The model proposes tool calls; it cannot open paths outside the selected workspace capability.
- **Workspace is capability-relative** (cap-std / beneath-root resolution), not “canonicalize then hope.”
- **Credentials** come only from the env var named in config — never from another agent’s store, never from TOML secrets.
- **Transport policy:** numeric loopback HTTP or remote HTTPS; redirects disabled; ambient proxies disabled by default in the verified design (enterprise proxy = explicit config, not ambient).
- **Text-only mode:** no tool schemas, no execution.
- **Verification tools are still execution** in the broader harness handbook sense; this agent simply does not expose write/shell tools yet.

### 2.5 Budgets and stops

Designed limits (README / agent constants; CLI may hard-wire defaults):

- Model rounds (e.g. 10)
- Total tool calls (e.g. 40)
- Wall-clock deadline (e.g. 180s)
- Repeated identical tool-batch breaker
- Context/request, response, and tool-result byte caps
- Per-tool scan/read ceilings (`READ_LIMIT`, `LIST_SCAN_LIMIT`, `SEARCH_SCAN_LIMIT`, etc.)

**Contract intent:** every run ends with an explicit stop — not a silent success after a budget cut.

### 2.6 What the dirty WIP is trying to add

Uncommitted work on the Mac tree (not stamped) appears to push toward:

- Real `StopReason` / `RunReport` types in `agent.rs`
- Richer `ToolErrorKind` and higher scan limits in `tools.rs`
- Split protocol adapters under `src/adapters/`
- `redact.rs` and `trace.rs`
- Expanded acceptance / protocol tests

Until that lands under a stamp, treat **`9baa811` as the verified public baseline** and the dirty tree as experimental.

---

## 3. Current state

### 3.1 Repository facts

| Item | Value |
|---|---|
| Branch | `feat/complete-read-only-agent` |
| HEAD | `9baa811` — *Generalize provenance references before going public* |
| Aligns with | `main`, `origin/main` |
| Remote | `Jimthetaxguy/min-agent-rs` |
| Diffstat (dirty) | ~2613 insertions / 648 deletions across 8 tracked paths |
| Untracked | `adapters/`, `redact.rs`, `trace.rs` |
| Tarball | `~/code/min-agent-rs-source.tar.gz` (~30K) |

### 3.2 Companion documents

| Doc | Path |
|---|---|
| Verified recap + Monty decision | `~/Downloads/min-agent  Verified Recap, Corrected Findings, and Monty Decision.md` |
| Council verification / backlog | `~/code/_working-files/20260918-191431-claude-min-agent-council-verification.md` |
| Design note | `~/code/_working-files/minimal-rust-agent-design.md` |
| In-repo verification / provenance | `docs/verification.md`, `docs/provenance.md` |

### 3.3 Verification record (`9baa811`)

- **Linux:** fmt, clippy `-D warnings`, `cargo test --locked`, Python acceptance 11/11.
- **Apple Silicon:** one sandboxed run blocked loopback bind; a later repo-root run passed full tests + acceptance.
- **Honest limits:** no paid inference; Chat Completions only in landed slice; no adversarial workspace mutator; exclusions ≠ secret scanner; no release vuln audit.

### 3.4 Known defects on the verified baseline

(From Downloads recap + code probe; some may already be partially addressed in dirty WIP — re-verify before claiming fixed.)

1. **Fail-closed too broad:** ordinary tool errors abort `run` via `?`; model cannot observe/recover.
2. **Evidence loss:** error paths drop transcript / partial answer (glossary “typed stops” were anyhow prefixes on baseline).
3. **Tool quality:** unsorted dir iteration + scan ceilings → nondeterministic truncation; byte pagination + lossy UTF-8; output cap after JSON serialize.
4. **`Prepared` public seam:** embedder can mint prepared values without `prepare` (bypasses arg bounds; path rechecks remain).
5. **Transport:** single shot; aggressive no-proxy; narrow retry not present.

### 3.5 Monty

**Deferred.** No live evidence that model rounds are the bottleneck. Code-mode would change the batch-prevalidation invariant and pull in a Tokio-centric pool runtime. Strategic upside (keep intermediate data out of provider context) remains real — after the read-only loop returns recoverable results.

---

## 4. Insights

1. **The load-bearing design is the result/error model, not the model brand.** Until tool failures are structured results, every new substrate (Monty, more adapters, more tools) measures harness bugs.

2. **“Read-only” is a product policy, not a full confidentiality proof.** There is no arbitrary attacker-chosen network sink in the tool layer, but workspace bytes still flow to the configured provider. Treat live tests as disclosure to that processor.

3. **Prevalidate-then-execute is the right host seam** for native tool batches. Keep it. Monty suspensions need the same discipline per callback.

4. **Determinism is a feature.** Unsorted `read_dir` makes acceptance flakes and user-visible “search missed it” failures. Sort + cursor beats “raise the limit.”

5. **Child CLIs are a different trust boundary.** A Rust `Workspace` wrapper does not confine an external coding-agent subprocess’s ambient FS/network. Document trust or confine at the OS/job level (cap-std’s own caveat for arbitrary code).

6. **Separate reusable capacity from consumed resources.** Canceling a future must not release a process slot while a child still runs; tokens/bytes already spent stay spent.

7. **Impulse `#65` is not this work.** Ion claim-gate ≠ staged-workspace failure demo ≠ min-agent `RunReport` repair. Keep lanes separate.

8. **Dirty WIP may already be chasing (1)–(2).** Do not double-implement. Diff the uncommitted tree against the defect list before starting a parallel rewrite.

---

## 5. Potential use cases

| Use case | Fit | Notes |
|---|---|---|
| Local repo Q&A on a trusted checkout | **Strong** | Primary design point |
| Offline `doctor` / config validation before spend | **Strong** | Already present |
| CI “explain this diff/tree” with fake or cheap models | **Good** | Loopback tests show the pattern |
| Embed as a library inside another Rust host | **Good** | Public `run` / `ModelClient`; fix `Prepared` opacity first |
| OpenRouter / LiteLLM / local OpenAI-compatible endpoints | **Target** | Configured Chat Completions; live cert not claimed |
| Read-only pre-pass before a write-capable agent | **Good** | Reduces blind writes; keep budgets tight |
| Secret-safe exploration of mixed workspaces | **Partial** | Need redaction + honest disclosure labeling (WIP `redact`) |
| Multi-agent swarm / long-running daemon | **Poor today** | No durable session journal, no supervisor, no write tools |
| Untrusted tenant isolation | **Poor today** | Not a sandbox product; host process trust assumed |
| Monty/code-mode refactor agent | **Future** | After recoverable tools + runtime boundary |

---

## 6. Recommended next writes (stamp-gated)

1. Inventory dirty WIP vs defect list (keep / split / discard) — **before** parallel rewrite.  
2. Land typed `RunReport` + recoverable tool errors (if not already complete in WIP).  
3. Deterministic list/search + UTF-8-safe read + truncate-to-fit.  
4. Opaque `Prepared`.  
5. Adapter fixtures for Responses/Messages.  
6. Monty only after (2) and an explicit runtime boundary design.

**Suggested owner:** @Impulse or @loops after James stamps. ROSA stays on `#50`.

---

## Appendix A — Building a similar concept in Python

This appendix sketches how to recreate the **same architecture and contracts** in Python — not a line rewrite of the Rust crate. Goal: same host-owned authority story, same budgets, same prevalidate-then-execute seam.

### A.1 Package shape

```
min_agent_py/
  pyproject.toml
  src/min_agent/
    __init__.py
    config.py          # connections, profiles, protocol enum
    model.py           # Protocol-agnostic turns + ModelClient Protocol
    adapters/
      chat.py          # Chat Completions request/parse
      responses.py     # optional
      messages.py      # optional
    tools.py           # Workspace, prepare/execute, tool specs
    agent.py           # Budget, StopReason, RunReport, run()
    redact.py          # optional
    trace.py           # optional
    cli.py             # typer/click entry
  tests/
    test_protocol.py
    test_workspace.py
    acceptance.py      # loopback fake model
```

Use a `src/` layout, `hatchling` or `uv` build, Python 3.11+. Pin deps in `uv.lock`.

### A.2 Mapping Rust → Python

| Rust | Python |
|---|---|
| `serde` structs | `pydantic` v2 models (or `msgspec` for speed) |
| `anyhow` / typed enums | `Exception` hierarchy **plus** returned `RunReport` (do not use exceptions for recoverable tool feedback) |
| `cap-std` Workspace | Start with `pathlib` + resolved-root checks; for stronger confinement research `cap` bindings or run tools in a subprocess with root jail — **document the trust gap** |
| `reqwest` blocking | `httpx.Client` (sync) with explicit timeouts; or `httpx` async behind a single runtime owned by adapters |
| `clap` | `typer` |
| `cargo test` + Python acceptance | `pytest` + an acceptance script that spawns a fake HTTP server (`http.server` or `pytest-httpserver`) |

### A.3 Critical contract: results vs exceptions

```python
class ToolErrorKind(str, Enum):
    NOT_FOUND = "not_found"
    DENIED = "denied"
    INVALID_ARGS = "invalid_args"
    TRUNCATED = "truncated"
    SCAN_LIMIT = "scan_limit"
    DEADLINE = "deadline"

@dataclass
class ToolOutput:
    ok: bool
    content: dict
    error: ToolErrorKind | None = None

@dataclass
class RunReport:
    outcome: Literal["completed", "stopped"]
    stop: StopReason | None
    answer: str | None
    rounds: int
    tool_calls: int
    usage: Usage
    items: list[Item]  # preserve evidence
```

**Rule:** `prepare`/`execute` return `ToolOutput`. Only harness faults raise. The agent loop appends tool errors as tool-result messages so the model can recover.

### A.4 Agent loop sketch

```python
def run(client: ModelClient, workspace: Workspace, opts: RunOptions) -> RunReport:
    budget = Budget.from_options(opts)
    items: list[Item] = [Item.user(opts.prompt)]
    while True:
        if (stop := budget.check()) is not None:
            return RunReport.stopped(stop, items=items, ...)
        turn = client.complete(system=SYSTEM, items=items, tools=definitions())
        budget.note_model(turn)
        if not turn.tool_calls:
            return RunReport.completed(turn.text, items=items, ...)
        batch = validate_batch(turn.tool_calls)  # protocol abort if invalid
        for call in batch:
            prepared = workspace.prepare(call)   # may return ToolOutput error
            out = prepared if isinstance(prepared, ToolOutput) else workspace.execute(prepared, budget.deadline)
            items.append(Item.tool_result(call.id, out))
            budget.note_tool(call, out)
```

### A.5 Workspace authority in Python

Minimum viable (honest about limits):

```python
class Workspace:
    def __init__(self, root: Path):
        self.root = root.resolve()
        if not self.root.is_dir():
            raise HarnessFault(f"missing workspace: {self.root}")

    def resolve(self, rel: str) -> Path:
        if Path(rel).is_absolute():
            raise ToolError(ToolErrorKind.DENIED, "absolute paths forbidden")
        cand = (self.root / rel).resolve()
        if not cand.is_relative_to(self.root):  # py3.9+: use try/commonpath
            raise ToolError(ToolErrorKind.DENIED, "path escapes workspace")
        return cand
```

**Stronger options (pick explicitly):**

- Run tool execution in a **subprocess** with `preexec_fn` / job object / container and a tight FS view.
- Use OS features (macOS sandbox-exec profiles, Linux user namespaces) — heavy; document instead of pretending `pathlib` is cap-std.

### A.6 Fake-provider acceptance test

Mirror the Rust acceptance driver:

1. Start loopback HTTP server that speaks Chat Completions and returns scripted tool calls.
2. Create a temp workspace with fixtures (`.env`, symlink, deep tree, binary/NUL file).
3. Assert: protocol violations stop; missing file returns tool error **without** killing evidence; scan limits surface `next_path`; budgets produce `RunReport.stopped(...)`.

### A.7 Suggested Python dependency set

- `pydantic` or `msgspec`
- `httpx`
- `typer`
- `pytest`, `pytest-httpserver` (or custom fake)
- optional: `rich` for CLI (keep core library free of it)

Avoid pulling LangChain/LlamaIndex as the core loop — they invert the “host owns authority” story unless heavily wrapped. If used at all, treat them as optional adapters behind `ModelClient`.

### A.8 What to copy from Rust min-agent (process, not code)

1. Honest verification.md (what ran where; what was not claimed).  
2. Provenance.md (inspired-by vs copied-from).  
3. Acceptance suite as product semantics, not just coverage.  
4. Stamp discipline: no Monty / write tools until recoverable read-only loop is true.  
5. Separate lanes from Impulse claim-gate and ROSA `#50`.

### A.9 First Python milestone (one failure path)

Demonstrate end-to-end:

1. Model requests `read_file` on a missing path.  
2. Tool returns structured `NOT_FOUND`.  
3. Model recovers and reads a valid path.  
4. Hit a scan limit with deterministic cursor.  
5. Cancel/deadline mid-run → `RunReport` retains transcript.  

That single path teaches more than adding another framework layer.

---

## Sources

- Live tree `~/code/min-agent-rs` @ `9baa811` + dirty WIP inspection (2026-09-18 evening)
- `README.md`, `CONTEXT.md`, `docs/verification.md`, `docs/provenance.md`
- `~/Downloads/min-agent  Verified Recap, Corrected Findings, and Monty Decision.md`
- `~/code/_working-files/20260918-191431-claude-min-agent-council-verification.md`
