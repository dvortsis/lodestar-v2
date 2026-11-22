# X-cmp0-polish decisions

Follow-ups from the CMP-0 review (#548), recorded in `runner/slices.yaml`'s `X-cmp0-polish` entry. They build on `2026-11-06-CMP-0.md`. Every item tightens a check, and none loosens an owner decision.

- **FlareSolverr names are compared whole, and such an entry needs only `challenge_solver`** (owner §2.47, Q21).
  - **What `main` already did:** since 9dd3c54d it compared the id, the container name and the image's last segment exactly. So `my-flaresolverr-fork` was already another companion. A regression test now pins that.
  - **What's new, a runner-approved refinement (owner §2.33; runner, 2026-11-07T05:15Z):** an entry named FlareSolverr was accepted if `needed_for` *contained* `challenge_solver`. So `["challenge_solver", "indexer_manager"]` passed, and the entry could pick up another role's defaults. Now `needed_for` must be exactly `["challenge_solver"]`, reported at `.needed_for`.
  - **Only the image's last segment counts, on purpose.** `ghcr.io/flaresolverr/solver` isn't FlareSolverr: an owner's other images are other programs. A test pins that choice.
- **A path refuses an escape wherever it refuses the literal byte, in either case.** Both `RequestPath` and `EnteredUrl`'s base path now decode every `%` escape and refuse:
  - `;` (`%3b`), the review's case;
  - the other bytes that end or split a path once a server decodes it: `?` (`%3f`), `#` (`%23`), `/` (`%2f`), `\` (`%5c`) and `%` (`%25`). The last three were refused already;
  - control bytes (`%00`–`%1f`, `%7f`), in the query too. Before, only `%00` was refused;
  - any byte from `0x80`, as before.

  **Allowed on purpose:** the escapes of printable bytes that are refused literally but mean nothing to a path once decoded: space, `"`, `<`, `>`, `^`, `` ` ``, `{`, `|`, `}`, `[`, `]` and `@`. An escape is how a path carries those bytes as data (`/a%20b`). In the query, `;` and its escape are data, not path parameters, so they're allowed.
- **Config templates are tied to the companion's id as well as its role.** `qbittorrent_conf` needs the `qbittorrent` entry and `prowlarr_config_xml` needs the `prowlarr` entry. Another download client or indexer manager is refused, because the writer for one program's config file is wrong for another.
- **A release folder is exactly `/<owner>/<project>/releases/download/`,** and a pin inside it is exactly `<tag>/<file>`. Before, the rule was "at least two segments and a closing `/`". That accepted `/<owner>/<project>/archive/` (source archives, not published builds) and extra segments before `releases`. The folder is compared exactly, so `Releases/Download` is refused, even though GitHub itself doesn't care about the case of owner and project names.
  - **For CMP-13's pin tool:** a tag that contains `/` (`release/v1`) can't be pinned, because a pin is `<tag>/<file>` with no deeper path. Today's upstreams don't use such tags.
- **The `compile_fail` doctests name their error codes:** E0624 (`ConfigPlan::new` is private), E0616 (`Container::image` is private), E0596 (a borrowed `Entry` can't be changed) and E0599 (`Artifact` isn't `Clone`).
  - **Each is minimal:** one offending line, and everything else compiles. So the intended error is the only one it can produce.
  - **Each code was checked by hand** with `RUSTC_BOOTSTRAP=1 cargo test -p lodestar-companions --doc`.
  - **The limit:** stable rustdoc reads the codes but doesn't check them; only nightly does. A `compile_fail,E9999` doctest passes under plain `cargo test` and fails only under `RUSTC_BOOTSTRAP=1`. So on CI the codes are documentation.
  - **CI won't set `RUSTC_BOOTSTRAP` (runner, 2026-11-07T05:15Z):** it would unlock nightly-only behaviour on the stable toolchain in the required pipeline. If a tool-backed check is ever wanted, the follow-up is a `trybuild`-style UI test in companions' dev-dependencies.
- **`ConfigPlan::for_tests` exists only under the companions crate's `testing` feature** (CMP-5g).
  - **Why a feature rather than `#[doc(hidden)]`:** a hidden constructor is still public. Any crate could call it in a release build and forge a plan, which would undo CMP-0's "a plan can't be forged".
  - **Why this feature:** it's the one `Catalogue::from_json` already sits behind. Only `[dev-dependencies]` enable it, and resolver 3 keeps dev-dependency features out of the binary's build. `cargo tree -e features -p lodestar` shows only `lodestar-companions feature "default"`.
  - **Runner-approved on those conditions** (2026-11-07T05:15Z): only `[dev-dependencies]` enable it, and never the `lodestar` package itself.
  - **Guarded in CI:** `ci.yml`'s "testing seams stay out of the binary" step now covers companions too (see the owns additions below).
  - **No feature is added to the `lodestar` package.**
- **Two owns additions (runner decision, §2.33, 2026-11-07T07:02Z):**
  - **`.github/workflows/ci.yml`, the "testing seams stay out of the binary" step only.** Its pattern now includes `lodestar-companions feature "testing"`, so a normal dependency that enabled the feature, and with it `ConfigPlan::for_tests` and `Catalogue::from_json`, would fail CI.
  - **`crates/companions/README.md`:** the `native.release` row now states the exact folder shape and the `<tag>/<file>` pin rule.
