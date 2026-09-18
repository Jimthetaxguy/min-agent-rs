# Provenance and reuse

This initial slice is independently implemented. No implementation source was
copied from IMPULSE, Composure, Codex, Grok Build, or fx; the references below
informed the contracts. This distinction is intentional, not a claim of code
extraction.

- IMPULSE `impulse-rs/src/loop_contract.rs`, observed at `f882252efd59`:
  round/wall-clock limits and explicit non-success stopping. The initial defaults
  follow the inspected Ion values. The local package says MIT while its LICENSE
  contains Apache-2.0, so source extraction is blocked pending reconciliation.
- Composure `crates/agent/src/backend.rs` and `openai.rs`, inside the agent-tools
  repo observed at `035794bfc6cc`: one-turn model boundary and preservation of
  opaque continuation. This slice does not reuse the Anthropic-canonical types.
- [fx](https://github.com/vercel-labs/fx): thin CLI plus embeddable core and offline
  diagnostics.
- [Grok tool taxonomy](https://github.com/xai-org/grok-build/blob/a28ee2b2063426e8816e380ccea528b9de95e5da/crates/codegen/xai-grok-tools/src/tool_taxonomy.rs):
  explicit tool capability classification.
- [Codex provider interface](https://github.com/openai/codex/blob/7498521d288b9b3b96ffba4eedf089d8d6e06a84/codex-rs/model-provider/src/provider.rs):
  provider capability boundaries.
- [OpenRouter wire contract](https://openrouter.ai/docs/api/reference/overview)
  and [LiteLLM proxy compatibility](https://docs.litellm.ai/docs/proxy/user_keys):
  configurable Chat Completions endpoints, native tool-call/result pairing.

No ROSA proprietary source was copied. Dependencies retain their own licenses;
Cargo.lock pins the resolved graph after the first verified build.
