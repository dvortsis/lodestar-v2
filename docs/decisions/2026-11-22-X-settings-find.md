# X-settings-find decisions

Finding a setting, from the UI review of 2026-11-13 (I12, I18, U29, P3, P17, and the non-conflicting part of U1; P4 is deferred, below). The runner's calls are under `docs/owner-decisions.md` §2.26 and flagged to the owner (§2.19). None of this changes what a setting does or the save pipeline, and there are no server changes.

## Field search (§10.10.3, I12)
- **One index, built when needed.** `shell/find.ts` builds it the first time the Settings box is focused or the command palette is searched, from the schema index and the section message maps the form engine already loads on demand. There is no build-time generator. The shell carries none of it: the box is a plain input until it is focused, and everything else is in one lazy chunk (gated by "Settings find (on demand)").
- **What it indexes:** each field's label, help, group, screen title, `config.toml` key, `LODESTAR_*` name and keywords, and each screen's title and purpose. Matching is case and accent insensitive, by word prefix for Latin text, and by substring for Chinese, Japanese and Korean, which have no spaces to prefix by. A label match ranks before a help or key match.
- **Secrets are never read out.** The index holds text from the schema and the messages, never a value. A result's value is read only when it is shown, and a field the schema marks write-only, or that holds a credential, shows "Set" or "Not set" and nothing else. A unit test runs over every such field of the golden schema.
- **A result jumps to the field.** The URL is `/settings/<screen>#<field>`, which the screens already resolve: they expand Advanced, scroll to the field and focus its control. After the jump the field is outlined for about two seconds (a fade, or a still outline under reduced motion).
- **The box is a combobox over a listbox** (WAI-ARIA 1.2), so it works by keyboard alone: `/` focuses it on a Settings page (a `settings.find` command in a `settings` scope, so it shows in the shortcuts help and never shares the key with Search's `/`), ↑/↓ move, Enter opens, Esc clears.
- **The palette** offers `Settings: <label>` entries from the same index, after its commands. It loads the index on the first search, and a signed-out palette shows nothing extra and no error. There are no direct-change commands ("Change theme"): they would change settings from the palette, which is out of scope.
- **Not done:** the "search the help pages" offer, because the app has no Help route yet (§10.10.3 says it offers one); and per-field entries for the hand-built screens, which are found by their title and purpose. Each would add bytes to the first view of every Settings screen. Every registry entry is reachable, and a test checks it.
- **Status badges on the navigation are left out:** the shell has no status data on first view, and a badge would need a new request.

## The Overview (U29, U1)
- **Start here is new.** The Overview had Needs attention and Changed from default but no Start here (§10.10.3). It lists the download client, notifications, updates, backups and the admin password, each with one action. The state of the first three is read from the section values the Overview already loads, with no new request: no download client is "Not set up", no notification channel is "In the app only", an update mode that is off or still the default is "Off". Backups and the password show only their action, because no value the Overview reads says where they stand.
- **Skipped first-run steps** (the wizard's own progress, kept in the tab) join the list as one row each, unless a row above already covers the screen. The wizard itself is unchanged.
- **Below 1024 px** the Overview reads: the search box, Needs attention, Start here, Changed from default, then the list of screens. The list used to come first, so Needs attention sat below 21 links.

## Settings that do nothing (I18)
Checked on a real install (`lodestar serve`, claimed, the API probed):
- **Hidden:** `transport.utp` and `transport.fallback_to_tcp`. The server's own comment (`settings/transport.rs`) says they are "stored and shown only; they change nothing but what's saved". They return when they act. `INERT` in `shell/screens.ts` lists them.
- **Kept:** Companions answers 503 "Companions aren't available in this process" on a plain run, but a Docker install with the companion service can have it, and hiding the screen would need a request on every Settings first view; its screen explains the state. Network & VPN shows real values (DHT port, bind address, secure cookies), so it stays, and its fields are findable.
- **Empty groups:** Index (Search & ranking) and Cadence (Trackers) hold only Advanced fields. The heading is skipped when a group has no essentials, and the Advanced disclosure stays.

## Smaller changes
- **P3:** the current item in the navigation uses `--color-selected`, and Appearance, Keyboard, Sources, Jobs and Blocklists get their own icons (one lazy module, `shell/nav-icons.ts`; the gear stays for General).
- **P4 is deferred** (runner, 2026-11-22): a switch's empty label row needs a layout change in `SettingField.svelte`, which every Settings first view, the Alert route and the Welcome route carry. It measured about 100–120 B on each of those and about 20 B of global CSS, which would have meant raising eleven limits. The runner queues X-field-layout to do it at no net size.
- **P17:** the primary button of Change password, Create key and the proxy form is a secondary button until its form has something typed.

## Size
New code is in the lazy chunk, which measures 5.69 kB gzip ("Settings find (on demand)"). What stays in the first view (the box wiring, the `/` key, the I18 fixes and the stylesheet) adds 0.1 to 0.3 kB to the Settings entries, which had 0.03 kB of headroom. The raises are recorded in the PR description.
