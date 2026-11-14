# X-fuzz-gaps decisions

Follows the 2026-11-14 parser review (the fuzz and property-test gaps sent to the runner). Fuzz inputs and one new target; no product code.

## Calls
- **A `peer_wire` target** for `lodestar_peer_wire::message`: the BEP 10 extended handshake, BEP 9 `ut_metadata` messages and BEP 11 `ut_pex`. These read what a peer sends during a fetch and had unit tests only. It checks no panics, that an extension id is never 0, that a client name is at most 64 characters, that a metadata piece is the rest of its own message, and that a peer list holds no more peers than its bytes allow and survives `pex_message`.
- **Seeds for `bencode`, `krpc`, `tokenizer` and `peer_wire`** in `fuzz/seeds/<target>/`. CI's `fuzz` job already passes that folder when it exists, so no workflow changes.
  - `krpc`: the BEP 5 messages (each query and response, an error), BEP 51 `sample_infohashes`, and the optional keys (`want`, `scrape`, `noseed`, `ip`, `ro`, `v`, `nodes6`).
  - `bencode`: each type; the edges of integers and string lengths; nesting; duplicate and unsorted keys; trailing bytes.
  - `tokenizer`: names in Latin, CJK, Hangul, Cyrillic, Arabic and Greek; combining marks, full-width forms, emoji and zero-width characters; a 300-character word.
- **Why seeds matter for a 60-second run:** CI gives each target 60 s from an empty corpus. For `krpc` that reached 371 coverage points here; from the seeds, 930 (same build, same 60 s, about 3,000,000 executions each).

## Runs (build VM, nightly, 60 s each, from the seeds)
| Target | Executions | Coverage points | Result |
|---|---|---|---|
| `peer_wire` | 1,150,903 | 574 | no crash |
| `krpc` | 3,032,092 | 930 | no crash |
| `bencode` | 1,516,569 | 593 | no crash |
| `tokenizer` | 648,785 | 588 | no crash |

These are short runs. The hour for each target that M8-SEC-1 asks for is still owed, and runs in a review session's own container, never in CI (`docs/security/v1/README.md` §2 step 6).

## Left out
- **Dictionaries** (`-dict=`): CI's `fuzz` job passes none, and adding the flag is a change to `.github/workflows/ci.yml`, which is the runner's. With these seeds the tokens a dictionary would hold are already in the corpus.
- **`metainfo` seeds near its limits:** X-metainfo01-fix (#862) added them (`budget-*.info`).
- **Targets for the fetch state machine and `UtpSocket::feed` with its table,** and property tests for the query AST from JSON and long operator chains: follow-ups, each needs code in its own crate's tests.
