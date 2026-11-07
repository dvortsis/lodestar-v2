# X-cmp0-polish decisions

Follow-ups from the CMP-0 review (#548), recorded in `runner/slices.yaml`'s `X-cmp0-polish` entry. They build on `2026-11-06-CMP-0.md`. Every item tightens a check, and none loosens an owner decision.

- **FlareSolverr names are compared whole, and such an entry needs only `challenge_solver`** (owner §2.47, Q21).
  - **What `main` already did:** since 9dd3c54d it compared the id, the container name and the image's last segment exactly. So `my-flaresolverr-fork` was already another companion. A regression test now pins that.
  - **What's new:** an entry named FlareSolverr was accepted if `needed_for` *contained* `challenge_solver`. So `["challenge_solver", "indexer_manager"]` passed, and the entry could pick up another role's defaults. Now `needed_for` must be exactly `["challenge_solver"]`.
- **A path refuses an escape wherever it refuses the literal byte, in either case.** Both `RequestPath` and `EnteredUrl`'s base path now decode every `%` escape and refuse:
  - `;` (`%3b`), the review's case;
  - the other bytes that end or split a path once a server decodes it: `?` (`%3f`), `#` (`%23`), `/` (`%2f`), `\` (`%5c`) and `%` (`%25`). The last three were refused already;
  - control bytes (`%00`–`%1f`, `%7f`), in the query too. Before, only `%00` was refused;
  - any byte from `0x80`, as before.

  **Allowed on purpose:** the escapes of printable bytes that are refused literally but mean nothing to a path once decoded: space, `"`, `<`, `>`, `^`, `` ` ``, `{`, `|`, `}`, `[`, `]` and `@`. An escape is how a path carries those bytes as data (`/a%20b`). In the query, `;` and its escape are data, not path parameters, so they're allowed.
- **Config templates are tied to the companion's id as well as its role.** `qbittorrent_conf` needs the `qbittorrent` entry and `prowlarr_config_xml` needs the `prowlarr` entry. Another download client or indexer manager is refused, because the writer for one program's config file is wrong for another.
- **A release folder is exactly `/<owner>/<project>/releases/download/`,** and a pin inside it is exactly `<tag>/<file>`. Before, the rule was "at least two segments and a closing `/`". That accepted `/<owner>/<project>/archive/` (source archives, not published builds) and extra segments before `releases`. Case matters, as it does in GitHub's paths.
- **The `compile_fail` doctests name their error codes:** E0624 (`ConfigPlan::new` is private), E0616 (`Container::image` is private), E0596 (a borrowed `Entry` can't be changed) and E0599 (`Artifact` isn't `Clone`). Each code was checked against the real compiler output.
  - **The limit:** stable rustdoc reads the codes but doesn't check them; only nightly does. `RUSTC_BOOTSTRAP=1 cargo test -p lodestar-companions --doc` checks them on stable. A `compile_fail,E9999` doctest passes under plain `cargo test` and fails under that command.
  - **Running it in CI** is a one-line `ci.yml` step, outside this slice's owns. The runner decides whether it goes in this PR or a follow-up.
- **`ConfigPlan::for_tests` exists only under the companions crate's `testing` feature** (CMP-5g).
  - **Why a feature rather than `#[doc(hidden)]`:** a hidden constructor is still public. Any crate could call it in a release build and forge a plan, which would undo CMP-0's "a plan can't be forged".
  - **Why this feature:** it's the one `Catalogue::from_json` already sits behind. Only dev-dependencies enable it, and resolver 3 keeps dev-dependency features out of the binary's build. `cargo tree -e features -p lodestar` shows only `lodestar-companions feature "default"`.
  - **No feature is added to the `lodestar` package.**
