# min-agent-rs — current state report

**Date:** 2026-09-18 (evening ET)  
**Owner question:** James asked Harness Handlers to review Downloads + `~/code` for the min-agent RS build.  
**Repo:** https://github.com/Jimthetaxguy/min-agent-rs  
**Local path:** `/Users/jamespustorino/code/min-agent-rs`

---

## 1. What it is

`min-agent` is a small **read-only** Rust coding agent: one library, one CLI, one bounded tool loop, and one OpenAI Chat Completions adapter. It is framed as the first verified slice of an fx-inspired design — a walking skeleton, not a replacement for Codex, Grok Build, or fx.

**In scope today**

- Public `ModelClient` seam and `run`
- Explicit connection/profile config; credentials from named env vars only
- Workspace tools: `list_files`, `read_file`, literal `search_text` (cap-std / beneath-root)
- Native tool calls with complete-batch prevalidation
- Budgets: 10 model rounds, 40 calls, 180s wall clock, repeated-batch breaker, byte caps
- Offline `doctor`; text-only mode
- Fake loopback HTTP tests (no live paid inference in the verification record)

**Out of scope today (explicit)**

- Edits, shell, persistent sessions, MCP, streaming UI
- Live OpenRouter / LiteLLM certification
- Responses / Anthropic Messages adapters (planned; partial untracked work exists — see §3)
- Monty / code-mode substrate

---

## 2. Where the artifacts live

| Artifact | Path | Role |
|---|---|---|
| Active checkout | `~/code/min-agent-rs` | Working tree |
| Source tarball | `~/code/min-agent-rs-source.tar.gz` (~30K) | Snapshot beside the repo |
| Downloads recap | `~/Downloads/min-agent  Verified Recap, Corrected Findings, and Monty Decision.md` | Council/code review + Monty deferral |
| Design note | `~/code/_working-files/minimal-rust-agent-design.md` | Design background |
| Council verification | `~/code/_working-files/20260918-191431-claude-min-agent-council-verification.md` | Probe against `9baa811`; backlog |
| Harness handbook map (Impulse) | `IMPULSE-rs` `docs/plans/2026-09-18-rust-agentic-harness-map.md` (untracked) | Related contracts; not this repo |

Related but **not** min-agent-rs: `~/code/minimax-subagent-mcp`, ROSA / Impulse trees.

---

## 3. Git state (2026-09-18 evening)

| Item | Value |
|---|---|
| Branch | `feat/complete-read-only-agent` |
| HEAD | `9baa811` — *Generalize provenance references before going public* |
| Same tip as | `main`, `origin/main` |
| Remote | `https://github.com/Jimthetaxguy/min-agent-rs.git` |
| Stashes | none observed |

**Dirty (modified, not committed)** — large WIP on the landed tip:

| Path | Approx. delta (whole-file churn) |
|---|---|
| `src/agent.rs` | major (~847 lines touched in diffstat) |
| `src/tools.rs` | major |
| `src/model.rs` | major |
| `src/config.rs` | major |
| `src/main.rs` | moderate |
| `src/lib.rs` | small |
| `tests/protocol_roundtrip.rs` | major |
| `tests/acceptance.py` | moderate |

Diffstat summary: **~2613 insertions / 648 deletions** across those eight paths.

**Untracked (new modules on disk, not in `9baa811`):**

- `src/adapters/` — `mod.rs`, `chat.rs`, `messages.rs`, `responses.rs`
- `src/redact.rs`
- `src/trace.rs`

Rough line counts on the dirty tree: ~4k LOC across `src/` + adapters + protocol tests (including WIP).

**Interpretation:** verified slice is frozen at `9baa811`. The working tree looks like an in-progress second slice (adapters / redaction / tracing / agent-tools rewrite). It is **not** stamped, **not** committed, and should not be treated as the verified baseline until James says so.

---

## 4. What was verified at `9baa811`

From `docs/verification.md` (dated 2026-09-18):

**Linux (Rust 1.93.1):** fmt, clippy `-D warnings`, build `--locked`, 11 unit + 1 HTTP integration tests, Python acceptance 11/11.

**Apple Silicon (two runs):** first sandboxed Mac blocked loopback bind (HTTP test not passed there); second run from repo root passed full `cargo test --locked` + acceptance 11/11.

**Honest limits in the record**

- No real provider / paid inference
- Chat Completions only in the landed slice
- No adversary mutating the workspace
- Filename exclusions ≠ secret scanner
- No release benchmark / vuln audit

`docs/provenance.md`: independently implemented; contracts informed by private prototypes, fx, Grok tool taxonomy, Codex provider interface, OpenRouter/LiteLLM docs — no vendored proprietary source.

---

## 5. Verified product defects (load-bearing)

These come from the Downloads recap + Claude probe against `9baa811`. They are the reason Monty is deferred.

### 5.1 Error taxonomy is too coarse

Protocol violations correctly abort. **Ordinary tool failures also abort the whole run** via `?` in `run` (missing path, denied `.env`, output ceiling, etc.). The model never sees structured tool feedback to recover.

Needed classes:

| Class | Correct behavior |
|---|---|
| Protocol violation | Typed protocol stop |
| Recoverable tool feedback | Structured tool result back to the model |
| Terminal resource stop | `RunReport` + typed stop + partial evidence |
| Provider failure | Bounded retry policy, else report |
| Harness fault | Distinct fatal error |

### 5.2 Result model drops evidence

Errors discard transcript / partial answer / last assistant message. “Typed stop reasons” in the glossary are **anyhow string prefixes**, not enums. Need something like `RunReport` / `RunOutcome` / `StopReason` (shapes sketched in the Downloads recap).

### 5.3 Tool quality

- `list_files` / `search_text`: unsorted `read_dir`, stop after ~500 scanned entries → non-reproducible truncation; probe: 1000 files / 3 needles → 2 found.
- `read_file`: arbitrary byte offsets + `from_utf8_lossy` → multibyte corruption across pages.
- Output ceiling applied **after** JSON serialize → control-heavy payloads blow the cap and kill the run; should truncate-to-fit with metadata.

### 5.4 `Prepared` seam

Public `Prepared` + public `Deserialize` args + public `execute` let an embedder mint prepared values without `prepare`, bypassing argument bounds (path rechecks still apply). Make minting opaque to `Workspace::prepare` only.

### 5.5 Transport

Single blocking request; `no_proxy()` clears all proxies; no narrow 429/5xx retry; CLI hard-wires default budgets.

---

## 6. Monty decision (locked for now)

**Defer Monty.** No live-provider evidence that rounds are the bottleneck. Monty also breaks “validate entire tool batch before any effect” unless every suspension is authorized first. Pool/runtime is Tokio end-to-end — not a drop-in for the blocking client without an adapter boundary.

Strategic Monty upsides (less provider disclosure when scripts keep data inside the sandbox) remain future options **after** the read-only loop returns recoverable results.

---

## 7. Related harness contracts (Impulse lane)

James’s critique of the Rust agentic-harness HTML report (stale-attempt workspace+promote, child-CLI trust, cancel-vs-supervisor, reusable vs consumed budgets, observed vs durable journal) was appended to Impulse’s map. **`#65` (Ion claim-gate) does not close those gaps.** That work stays separate from min-agent-rs’s next product write.

---

## 8. Recommended next writes (stamp-gated)

Order matters. Do not start Monty or a fourth tree.

1. **Typed `RunReport` + stop enums** — preserve transcript on non-success.
2. **Recoverable tool results** — missing/denied/truncated become model-visible tool outcomes; rewrite acceptance tests that currently require abort-on-tool-error.
3. **Deterministic listing/search** — sorted traversal, cursor/`next_path`, visible scan ceiling.
4. **UTF-8-safe read pagination + truncate-to-fit output.**
5. **Opaque `Prepared` minting.**
6. **Then** decide whether dirty `adapters/` / `redact` / `trace` WIP is kept, split, or discarded — after reviewing the uncommitted diff, not before.
7. **Only later:** Responses/Messages adapters behind shared fixtures; Monty behind a dedicated runtime boundary.

**Suggested owner:** @Impulse or @loops (James stamps). ROSA is not the owner (`#50` billing+scope remains theirs).

---

## 9. Stamp board touchpoints

| Item | Status |
|---|---|
| Commit / push dirty min-agent-rs WIP | Needs James stamp |
| Product next write (RunReport + recoverable tools) | Needs James stamp to start |
| Impulse `#65` | Separate; open when James wants |
| ROSA `#50` | Held (billing + scope; path closed) |
| design-flow land | Separate De$igner stamp |

---

## 10. Sources

- Live tree: `~/code/min-agent-rs` @ `9baa811` + dirty WIP inspected 2026-09-18 evening
- `docs/verification.md`, `docs/provenance.md`, `README.md`, `CONTEXT.md`
- `~/Downloads/min-agent  Verified Recap, Corrected Findings, and Monty Decision.md`
- `~/code/_working-files/20260918-191431-claude-min-agent-council-verification.md`
