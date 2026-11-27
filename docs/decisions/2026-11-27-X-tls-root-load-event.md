# X-tls-root-load-event decisions (follow-up to #1125, X-sec-S8-25)

- **Two catalogued events, one emitted per load.** A catalogue event's level is fixed at declaration (`lodestar_log::catalogue!`), so the brief's "info when fine, warn otherwise" is two events with the same fields, and `tls::roots()` emits exactly one of them the first time it reads the OS store:
  - `integrations.tls_roots_loaded` (Info): roots were kept and nothing failed;
  - `integrations.tls_roots_degraded` (Warn): anything else.
- **Fields, counts and fixed words only:** `found`, `load_errors` (the number of errors `rustls-native-certs` reported, never their text, which can name a path), `unusable` (webpki refused it as a trust anchor), `duplicates`, `disallowed` (dropped by Windows' Disallowed list), `kept` (all `Size`), `store` (`read` or `unreadable`: the loader panicked) and `disallowed_list` (`read`, `unreadable`, or `none` off Windows). No subject, issuer, fingerprint, path or error text. With both lists read, `found = disallowed + duplicates + unusable + kept`.
- **What counts as a failure:** a load error, an unusable certificate, an unreadable store or Disallowed list, or no root kept. Duplicates and Disallowed drops are the store working as meant, so they alone don't warn.
- **Duplicates are counted among kept roots.** An unusable certificate that appears twice counts as unusable twice; the roots kept are unchanged from #1125.
- **An unreadable Disallowed list** keeps nothing and drops nothing by count: `found` can be above 0 with `kept` 0 and every drop count 0; `disallowed_list: unreadable` says why.
- **The guide's sentence** points at the `integrations.tls_roots_` prefix, since a healthy load whose roots just lack the site's root logs `tls_roots_loaded`, not `tls_roots_degraded`.
- **Tests (`tls::tests`):** counts for a mix of good, unparsable, duplicate and disallowed certificates; a fail-closed load (Disallowed list unreadable, loader panicked); a store with nothing usable, and an empty one; and a capture sink asserting each emitted record's fields are exactly the catalogued ones, only counts and fixed words, at the catalogued levels.
- **User guide:** section 8.6 (revision 3.6) names the `integrations.tls_roots_` lines to look for when every routed source says the certificate isn't trusted.
- **Catalogue golden:** `crates/lodestar/src/settings/golden/logs_catalog.txt`, re-blessed with `LODESTAR_BLESS=1`.
- **Settings → Logging names (runner-granted shared edits, 2026-11-27):** `web/src/lib/features/settings/logging/event-names/integrations.ts` (`npm run event-names`) and one `_n`/`_h` pair per event in that feature's `messages/en.json` and `zh.json`, which `event-labels.test.ts` requires.
- **Budget (runner, §2.26):** the integrations event-names chunk measured 1220 B against 1060 B; `web/.size-limit.mjs` sets it to 1520 B (measured + 300 B). The runner flags it to the owner.
