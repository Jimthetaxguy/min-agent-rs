You are performing a corrected, code-verified modernization assessment of Jimthetaxguy/min-agent-rs, checked against current main at commit 1d6eba0 (2026-10-22). Do not rely on prior ForgeScore, Intent, PRD, or Architecture outputs for this repository. Several of those outputs contained factual errors. This prompt supersedes them.

## Ground truth (do not re-litigate)

### Already true in the code (do not flag as gaps)
- Transport policy is already the authoritative gate in `src/config.rs`. It rejects `http://localhost`, non-loopback HTTP, URL credentials, and query strings, and tests exist for each case. Any suggested fix that allows `localhost` for plaintext loosens the existing policy and must be rejected.
- `cargo deny` already runs in CI. `.github/workflows/ci.yml` has a `deny` job that runs on every pull request and every push to main.
- The real Rust import graph is clean except for one cycle:
  - `config`, `trace`, and `redact` import nothing from the crate.
  - `tools` imports only `redact` and `ToolSpec`.
  - `agent` imports only `model`, `tools`, and `trace` (never `config`).
  - The only real cycle is `model` <-> `adapters`, because `HttpModelClient` lives in `src/model.rs` and calls the adapters.
- `src/main.rs` is the only composition point that wires config to the runtime.
- Responses and Messages are already opt-in by construction: every connection must name its protocol explicitly. Do not invent a separate "beta packaging" workstream for them.
- A typed context-byte stop already exists (`Limit::ContextBytes`). The only missing piece is attributing which provider output caused the growth.
- RUSTSEC-2026-0381 and RUSTSEC-2026-0382 (TLS, published 2026-10-19 and 2026-10-20) are fixed in the lockfile on main by PR #7.
- Rust 1.99.1 has been out since 2026-10-13. Do not treat 1.99.0 as latest stable.

### Real defect that was missed
`read_file` has a paging bug that is still present on 1d6eba0:
- `read()` fills a page up to the full 32 KiB output cap. When the saved-result note is then added, `content` is trimmed but `next_offset` is not updated. A NUL-heavy page delivers 5,432 bytes while `next_offset` says 5,448, so 16 bytes are silently skipped.
- When the repeat-call warning is also attached, 5,417 bytes are delivered against the same `next_offset`, so 31 bytes are skipped.
- Every non-final page of a file larger than 8 KiB is marked truncated, saved to disk, and annotated with a `read_tool_result` note, even when nothing was cut. This nudges the model away from `next_offset`.
- There is no invariant test that `next_offset - offset == delivered length`.

An assessment that does not catch this is not a correctness check. Fixing this bug is higher priority than any structural modernization.

### Inflated items from prior analysis (do not expand scope around them)
- System Gravity 54 and the "deepest chain" were inflated by a cross-language name collision. Python `subprocess.run` in the acceptance harness was matched to Rust `agent::run` and then to `main.rs::execute`. The real graph does not contain that chain.
- The proposed 6-phase, ~8-week plan (self-hosted runner, OpenTelemetry export, central metadata store, beta packaging, final promotion) is larger than the problems that exist. Do not generate user stories, tests, or phases for those items.
- Newtypes around opaque provider payloads are optional. Replay-byte reporting does not require them.
- Rust 2024 lifetime-capture changes may need manual review. Do not assume `cargo fix --edition` handles every case cleanly.

## What the modernization must actually do

Produce a minimal, code-verified plan that does only the following, in this order:

1. Fix the `read_file` paging bug and add the invariant test (`next_offset - offset == delivered`). Separate "more of the file remains" from "output was capped", and only save and annotate when output was actually capped. Fit pages within the output limit minus the annotation reserve, as list and search already do. This is the only real correctness defect.
2. Break the single real cycle by moving `HttpModelClient` into a `transport.rs` module (or equivalent). Add a short allowlist test (or xtask) that fails if any module imports something outside a small permitted set. This is the useful part of an architecture check, without a large composition rewrite.
3. Report replay bytes per provider item in `RunReport` and the metadata-only trace, so the existing `Limit::ContextBytes` stop says what caused the growth. Newtypes are optional.
4. Audit the public surface (`lib.rs` currently exposes every module). Recommend narrowing only if embedding is a near-term goal; otherwise leave it.
5. Move to Rust 2024 edition and document an MSRV policy. Run the existing verification matrix (fmt, clippy, tests, cargo-deny, acceptance, held-out, size gate, MSRV and current stable). Flag any lifetime-capture or name-resolution changes that need manual review.

## Explicitly out of scope (do not generate work for these)
- Allowing `localhost` for plaintext HTTP.
- "Beta" packaging or feature flags for Responses and Messages (already opt-in).
- Self-hosted runner, OpenTelemetry export, central metadata store, object-store archive, secret-manager integration.
- Read cache, compression feature flag, or splitting the Python test helpers.
- A multi-crate split or full composition-seam rewrite. The monolith is correct; only the one cycle needs breaking.
- Any change that pushes the release binary above the existing 2,411,596-byte gate without explicit justification.

## Constraints that must be preserved
- Read-only by construction. No shell, no file mutation, no ambient filesystem authority.
- The workspace boundary in `src/tools.rs` (`cap-std`, path denial, symlink and hardlink rejection, blocked names, prepare/execute split) is the gold standard. Preserve it.
- Explicit configuration only. No silent discovery of profiles, credentials, or proxies.
- Metadata-only traces and summaries by default. Redaction remains a backstop, not a DLP system.
- Small binary and minimal dependencies. `ureq` + rustls stays. No Tokio or Hyper.
- The existing verification matrix must continue to pass.

## Output requirements
- Lead with the corrected findings: what was wrong, what was missed, what was inflated.
- Produce only the five work items above. Items 1 to 4 are Small; item 5 is Medium.
- For each item, state the concrete change, the invariant or test that proves it, and what must not change.
- Do not generate phases, user stories, or diagrams for out-of-scope items.
- Do not re-flag closed items (transport, cargo-deny, import graph beyond the single cycle).
- If you produce an Architecture or PRD artifact, mark the prior versions as superseded by this correction, and do not carry forward the incorrect transport gap, the inflated gravity chain, or the oversized later phases.

Treat this prompt as the authoritative correction. Prior Forge outputs for this repository that contradict it are obsolete.
