# X-strip-format-chars decisions

A hardening slice from the review of #366 (SRC-8). Feed titles and tags (`crates/sources-feeds/src/common.rs`) and digest fields (`crates/api/src/digest.rs`) removed controls and bidi controls but kept other invisible characters. A feed title `a\u{200B}c\u{FEFF}d\u{2028}e` kept U+200B, U+FEFF and U+2028. This slice builds on `2026-11-01-SRC-6.md` and `2026-11-02-SRC-8.md`, and on architecture §18.9 and §20.

- **One shared definition: `lodestar_metainfo::hidden_char`** (`crates/metainfo/src/display.rs`). It strips:
  - controls (C0, DEL, C1);
  - all of general category Cf, which includes U+00AD, U+200B–200F, U+2060–2064, U+FEFF and the bidi controls;
  - U+2028 and U+2029;
  - tag characters U+E0000–E007F, including the unassigned U+E0000 and U+E0002–E001F.
- **It lives in `lodestar-metainfo`** because that is the one crate both `lodestar-sources-feeds` and `lodestar-api` already depend on. The move adds no dependency edge. The runner was told before the file was added.
- **Cf is an explicit range table (Unicode 15.1, 21 ranges) with a binary search.** It adds no runtime dependency.
  - A test walks every code point and checks that the table plus the extra classes is exactly the set stripped. Cf comes from `unicode-properties` (general-category feature only), a dev-dependency that was already in `Cargo.lock` through `stringprep`.
  - When a later `unicode-properties` adds Cf characters, that test fails and names them.
- **Both callers use it, with nothing else changed.**
  - The Torznab and RSS title cleaning (`clean_title`) still removes these characters and then cuts the result to 1,024 B.
  - A Torznab tag that contains one is still dropped whole, as before.
  - The digest's `clean` removes them, then cuts the result to 64 characters with `…`.
  - The golden tests are unchanged and pass.
- **Joiners go too.** U+200D inside an emoji sequence is removed, so `👩‍💻` shows as `👩💻`, and subdivision flags (tag sequences) fall back to a plain black flag. A name that can't hide text is worth that cost. U+FE0F (a variation selector, category Mn) is kept.
- **Out of scope:** `crates/lodestar/src/logging/format.rs` has its own `unsafe_char`, which escapes characters in log lines rather than stripping them. This slice doesn't own that file and leaves it alone.
- **Filter matching was not changed. The runner decides on this as a content-safety question.**
  - `crates/filter/src/normalize.rs` `invisible()` already ignores U+00AD, U+200B–200F, U+202A–202E, U+2060–2064, U+2066–206F and U+FEFF. It also ignores some non-Cf fillers.
  - It does not ignore U+2028/2029, the tag characters, U+0600–0605, U+06DD, U+070F, U+0890–0891, U+08E2, U+FFF9–FFFB, U+110BD, U+110CD, U+13430–1343F, U+1BCA0–1BCA3 or U+1D173–1D17A. Any of these inside a word in a name that reaches the filter without passing through this cleaning (DHT names, for example) can still split that word.
  - Feed titles are cleaned before they are stored, so after this change they no longer carry any of these characters.
