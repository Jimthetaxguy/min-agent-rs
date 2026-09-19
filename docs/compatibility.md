# Compatibility matrix

Rows are recorded only from actual runs of `tests/live.rs` (opt-in, `--ignored`) against a
synthetic temporary workspace. The task plants the fact `HELIOTROPE-47` in
`docs/notes/launch.md` and asks for it, so a tool-using model must list, search, or read to
answer. "Pass" means the run completed, used tools, and quoted the fact exactly.
Unsupported combinations are recorded here, not worked around.

| Date | Protocol | Endpoint | Model | Result | Rounds | Tool calls (errors) | Tokens in/out |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 2026-09-18 | `openai_chat` | Ollama 0.34.1, loopback | `qwen3:8b` | Pass | 3 | 2 (1) | 2327 / 773 |
| 2026-09-18 | `openai_chat` | Ollama 0.34.1, loopback | `llama3.1:8b` | Fail | 2 | 1 (1) | 631 / 78 |

Notes:

- `qwen3:8b`: the first call failed with a tool error that was returned to the model, which
  corrected its request and found the fact. Under the earlier abort-on-tool-error loop this
  run would have stopped after one round.
- `llama3.1:8b`: sent `"limit":"20000"` (a string where the schema requires an integer),
  received an `invalid_arguments` result, then wrote its corrected call as prose JSON instead
  of a native tool call. The agent never parses prose into actions, so the prose became the
  final answer and the fact was not found. This is a model tool-calling limitation, not a
  transport failure; the run is recorded as a failed tool-using test.
- Not yet run: any hosted provider (OpenRouter, LiteLLM, OpenAI Responses, Anthropic
  Messages). Those adapters are verified against loopback HTTP fixtures only, and live runs
  send real data and cost money, so they wait for explicit authorization.
