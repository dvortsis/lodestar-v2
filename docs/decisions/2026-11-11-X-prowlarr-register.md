# X-prowlarr-register decisions

- **Prowlarr / Jackett is registered, off, behind the consent gate** (owner §2.55). `Services::start` builds it through `Prowlarr::at_start` from its saved settings and the API key stored at that moment. It's an Account source, off by default, and `consent_text_approved` refuses the Account class in this build, so it can't be turned on yet, the same as the torrent cache.
- **A store error at start-up fails the start.** The first version of the registration (9e7285e5) turned an unreadable `sources.config.prowlarr` into "unconfigured" with `.ok().flatten()`. Now the error stops `Services::start` with a message that names the setting and not its value. A source quietly registered without its saved address would hide a damaged store.
- **Legal Notice section 2: changes 1–3 of the owner-approved draft, word for word,** in both copies. Nothing else in the notice changes. Restricted and Overlay keep "None is built yet.", which now appears twice.
- **Still version 5, no second raise.** X-event-log-notice (#702) raised `NOTICE_VERSION` to 5 for §2.55's two changes. No release has been cut since (the newest tag is v0.5.1), so this text ships under the same version. If a release had shipped 5, this would have been 6 (§2.55).
- **`legal_facts.rs`** checks:
  - four registered plugins, Prowlarr's among them;
  - the approved Prowlarr phrases;
  - Prowlarr's manifest is `Consent::Account`, off by default and `finds_private`, read from its source because `MANIFEST` is crate-private;
  - the gate refuses `Consent::Account`;
  - its endpoints are the configured origin only;
  - "None is built yet." appears exactly twice.
- **The binary test from 9e7285e5 is back** (`consent_gate.rs`, `prowlarr_is_registered_off_and_its_settings_save`), unchanged. It checks that the plugin is registered and off, that hostile addresses are refused without echoing them, and that the key is never echoed, logged or stored unsealed. It also checks that the gate refuses every enable, and that saved settings apply at the next start.
