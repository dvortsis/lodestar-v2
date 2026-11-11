# rv-709 @f820eeba (X-prowlarr-register): MERGE
Fresh Opus review + security pass, 2026-11-11 (UTC). Blocking: none.
- Notice text matches main + draft changes 1-3 byte for byte; two copies identical; NOTICE_VERSION 5; no release between.
- Consent gate traced: Account consent, off by default, switch_off_unapproved before anything runs, egress checks source_enabled, no network at start, no secrets.key created without a key.
- webhooks_e2e: rename + ("prowlarr", false) + stricter asserts only.
- Gates: fmt, clippy, consent_gate 2, legal_facts 8, webhooks_e2e 2, services 8, api legal 3 all pass; no test-util/testing.
Important (follow-up): legal_facts.rs:265-275 "never downloads a .torrent from any indexer" is phrase-checked only; add a source check that prowlarr.rs never fetches item.link.
Nits: consent_gate.rs:311 line >100 cols; decisions heading lacks (#709); services.rs:347 `ID` -> prowlarr::ID.
