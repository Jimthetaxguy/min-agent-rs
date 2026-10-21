# Redaction scanner: remove repeated span searches

The scanner now walks the ordered PEM ranges with a forward cursor instead of
searching every accumulated credential range at every input position. Token
recognition, PEM recognition, final range sorting, and window masking are unchanged.

## Source lesson and current issue

The earlier review recorded in [verification.md](verification.md) found masking
bypasses through offsets, page splits, and search probes. Its context-window
redaction and existing regression tests are valuable behavior to retain.

The subsequent implementation still ran `ranges.iter().find(...)` at each scanner
position. Previously recognized tokens never cover that position: recognizing a
token already advances the position to its end. Rechecking those ranges adds work
without changing the result. For N synthetic tokens separated by newlines, the
old scanner performs N squared range-containment checks. A byte cap alone does
not cap the cost of those nested scans.

Only the original PEM prefix of the range vector needs membership checks. Those
PEM ranges are ordered and disjoint. Advancing a cursor through that prefix avoids
rechecking prior tokens and prior PEM blocks, while appending tokens preserves
the original range data for the unchanged final sort and masking code.

## Regression and scaling evidence

This change is based on `87c5e9732dfebf1a00e1b9b8c5c7b006e6c116ff`, including the
completed saved-result-page and atomic-summary reliability changes. That head's
scanner is identical to the earlier `b0249b6` scanner.

Two added tests exercise 768 independently expected masks and compare exact ranges
plus window bytes/counts against the frozen baseline scanner on 417 adversarial
inputs. The corpus includes adjacent tokens, multiple PEMs, token/PEM overlaps,
malformed and unterminated PEMs, certificates, UTF-8, invalid bytes, empty windows,
and offsets inside or beside matches. Recognition helpers remain shared because
this change concerns traversal; the existing literal format tests also remain.

Before changing production code, the opt-in release probe failed: four times as
many dense tokens took 13.3 times longer. The same probe passes after the change:
fourfold growth took 4.2 times longer. It is release-only and ignored by default
so ordinary test runs do not acquire a wall-clock timing dependency.

A separate paired source comparison compiled both implementations into one
process with Rust 1.98.1 on Apple Silicon macOS, using `opt-level=z`, fat LTO, and
one codegen unit. Measurements alternate old/new order and report the median of
nine batches of 32 scans:

| Synthetic input | Bytes | Baseline | Improved | Baseline / improved |
| --- | ---: | ---: | ---: | ---: |
| 4,096 tokens | 114,688 | 4.805 ms | 0.445 ms | 10.8x |
| 1,024 PEM blocks | 60,416 | 0.593 ms | 0.170 ms | 3.5x |
| Ordinary text | 143,360 | 1.120 ms | 1.132 ms | 0.99x |

In that paired comparison, fourfold dense-token input growth changed from 11.8x
to 4.2x time growth. Ordinary text was approximately 1–2% slower across the two
measured sizes. These are local synthetic scanner measurements, not end-to-end
agent speedups. PEM discovery and final sorting remain unchanged; this is not a
claim that every malformed-input path now has linear complexity.

## Validation and reproduction

All commands run from the repository root, with build output kept in the checkout:

```sh
export CARGO_TARGET_DIR=target
cargo fmt --check
cargo check --offline --locked --all-targets
cargo clippy --offline --locked --all-targets -- -D warnings
cargo build --offline --locked
cargo test --offline --locked
python3 tests/acceptance.py target/debug/min-agent
python3 tests/test_heldout_eval.py
python3 tests/heldout_eval.py target/debug/min-agent
cargo +1.88.0 check --offline --locked --all-targets
cargo deny --locked --offline check advisories bans licenses sources
CARGO_NET_OFFLINE=true ./scripts/check-release-size.sh
cargo test --offline --locked --release --test redaction_scaling -- --ignored --nocapture
```

The integrated baseline passed 83 tests; the final implementation passed 85
(74 library, 2 CLI, 9 synthetic HTTP), with the live-provider test still ignored.
All 27 Python acceptance checks and the opt-in release scaling probe passed.
Formatting, all-target checking, Clippy with warnings denied, and the offline
build passed. Localhost permission was needed for synthetic HTTP fixtures.

Pre-commit validation also passed all 10 scorecard controls, all five public
scripted regression tasks, and the Rust 1.88.0 all-target check. The release
binary was 1,668,672 bytes against the 2,411,596-byte limit on
`aarch64-apple-darwin`. Cargo deny passed with cached advisory data; its existing
duplicate `windows-sys` and yanked `yoke-derive` warnings remain.

The isolated delivery includes full logs, frozen scanner sources, compiler and
source hashes, and a reproducible paired-comparison script alongside its patch.
Live providers and other platforms were not exercised for this change.
