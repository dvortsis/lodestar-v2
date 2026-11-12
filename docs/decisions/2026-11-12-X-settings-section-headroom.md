# X-settings-section-headroom decisions

- **Language's navigation icon now loads on demand, like every other screen's icon.** Language was the only screen to import its Phosphor icon (`translate`) eagerly. The rule in `shell/screens.ts` says an icon goes through `iconLoad`, because an eager icon costs every Settings first view about 300–600 B.
  - The icon is now exported from `screens2/icons.ts`, the lazy module that already holds the Trackers and connection screens' icons.
  - `NavIcon` shows the gear until the module loads, and keeps the gear if the load fails. This is the same as Crawler, Network, Storage, Search, Trackers and the connection screens.
  - Nothing else reads a screen's icon.
  - Why: UP-1's `watch` section adds one `LOADERS` line, which put "Settings section (own JS + CSS)" 2 B over its 45 KB cap (§10.1, owner-set). A lazy `import.meta.glob` for `LOADERS` measured 64 B worse, so the runner opened this slice instead (runner §2.26, 2026-11-12).
- **Measured with `npx size-limit`, gzip, before → after:**
  - Settings section (own JS + CSS): 44979 → 44686 B, which leaves 314 B under the 45 KB cap. The limit is unchanged.
  - Settings overview: 14355 → 14062 B.
  - Every per-screen Settings first view: about 290–295 B smaller.
  - Settings connection screens (on demand): 12328 → 12576 B, because the icon now loads with that module. It stays under its 14.2 KB limit.
  - Chunk reshuffles of 1–3 B in some on-demand entries:
    - Settings updates parts 2955 → 2958 B;
    - Settings updates advanced 4444 → 4445 B;
    - Sources dialogs 38836 → 38839 B;
    - Sources config dialog 5081 → 5082 B.

    All stay under their limits.
  - No limit in `web/.size-limit.mjs` moved, and the gate's file sets are untouched.
