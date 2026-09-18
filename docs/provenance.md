# Provenance and reuse

This initial slice is independently implemented. No implementation source was
copied from any private prototype, Codex, Grok Build, or fx; the references
below informed the contracts. This distinction is intentional, not a claim of
code extraction.

- Private Rust agent prototypes of mine: round/wall-clock loop limits with
  explicit non-success stopping, and a one-turn model-boundary pattern that
  preserves opaque provider continuation without adopting any single vendor's
  canonical types. One of these prototypes declared MIT in its package manifest
  while its actual LICENSE file was Apache-2.0, so source extraction from it was
  treated as blocked pending reconciliation rather than resolved by assumption.
- [fx](https://github.com/vercel-labs/fx): thin CLI plus embeddable core and offline
  diagnostics.
- [Grok tool taxonomy](https://github.com/xai-org/grok-build/blob/a28ee2b2063426e8816e380ccea528b9de95e5da/crates/codegen/xai-grok-tools/src/tool_taxonomy.rs):
  explicit tool capability classification.
- [Codex provider interface](https://github.com/openai/codex/blob/7498521d288b9b3b96ffba4eedf089d8d6e06a84/codex-rs/model-provider/src/provider.rs):
  provider capability boundaries.
- [OpenRouter wire contract](https://openrouter.ai/docs/api/reference/overview)
  and [LiteLLM proxy compatibility](https://docs.litellm.ai/docs/proxy/user_keys):
  configurable Chat Completions endpoints, native tool-call/result pairing.

No other proprietary source was copied. Dependencies retain their own licenses;
Cargo.lock pins the resolved graph after the first verified build.
