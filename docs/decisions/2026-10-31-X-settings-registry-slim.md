# X-settings-registry-slim decisions

The Settings screen registry (`web/src/lib/features/settings/shell/screens.ts`) is loaded with every Settings first view. It carried each screen's purpose line, Network's notice and four eager Phosphor icons. This slice moves them off the first view. Flagged to the owner under §2.19: it reverses the runner's 45.1 KB raise for the Notifications first view (`2026-10-31-S-logs-2.md`, §2.26).

- **Purpose lines and notices load lazily** (§10.10.2, §10.10.8). An entry no longer has `purpose` or `notice`. Each registry file has one small `about.ts` (`ABOUT`, by screen id), which all its entries share through `about`. It loads when a generated screen opens, together with the form and the settings index, and when the Settings filter is first used. Reasons:
  - only a generated screen's own page shows its purpose and notice, and the filter matches purposes only after someone types;
  - one module per file keeps one preload list in the registry, not one per entry.

  A generated screen shows "Loading settings" until its purpose, form and index are all in, so the purpose line, notice and form appear together. If the `about` chunk can't load, the page shows the usual "Part of this page couldn't load" banner with Reload, because Network's notice explains why its fields are read-only. Until the filter's purposes arrive, or if they fail to load, the filter matches titles only.
- **Titles stay in the entries.** The navigation shows every title on first paint, so titles cost the same on every first view wherever they live. Moving them into the shell's chunk would save nothing (≈ 25 B per title either way, in en and zh). It would also mean every new screen edits the shell. So the registry keeps `id`, `group`, `title`, the icon loader and the body loader or sections. zh is unchanged: titles are still the Paraglide messages.
- **The generated screens' icons load after the navigation shows** (`iconLoad`, one `screens/icons.ts`), as the connection screens', Security's and Logging's already did: the gear holds their place. General keeps the gear, which every Settings page already loads. The four SVG paths were ≈ 1.3 KB of the registry chunk.
- **Size gates** (`web/.size-limit.mjs`). The overview and each hand-built screen's first view stop at the `about` modules. "Settings section" (a generated screen) counts them, because it loads them at mount. New entries, at measured + 10%: "Settings screen purposes (on demand)" 0.96 KB and "Settings screen icons (on demand)" 1.66 KB. The Notifications first view is back at 45 KB with the other hand-built screens, and its runner-raise comment is gone.
- **Measured, two builds each, `main` → this branch (bytes, gzipped):**

  | Entry | `main` | This branch | Limit |
  |---|---|---|---|
  | Settings overview | 12,645 / 12,628 | 11,048 / 11,048 | 45 KB |
  | Settings section | 44,006 / 43,965 | 43,603 / 43,597 | 45 KB |
  | Appearance first view | 29,241 / 29,211 | 27,776 / 27,769 | 45 KB |
  | Keyboard first view | 36,492 / 36,453 | 35,028 / 35,029 | 45 KB |
  | Language & region first view | 44,523 / 44,482 | 43,275 / 43,260 | 45 KB |
  | Integrations first view | 44,314 / 44,276 | 42,853 / 42,846 | 45 KB |
  | Notifications first view | 45,067 / 45,031 | 43,602 / 43,596 | 45.1 → 45 KB |
  | Insights first view | 44,803 / 44,767 | 43,336 / 43,332 | 45 KB |
  | Logging first view | 43,821 / 43,782 | 42,357 / 42,349 | 45 KB |
  | Security first view | 31,541 / 31,510 | 30,085 / 30,071 | 45 KB |
  | Alert route | 45,993 / 45,951 | 45,993 / 46,002 | 46 KB |
  | Search results (lazy) | 43,285 / 43,267 | 43,298 / 43,299 | 43.3 KB |

  Every Settings first view now has ≥ 1.39 KB headroom. Alert and Search results don't change: apart from the hashed chunk names in their import strings, their chunks are byte-identical between `main` and this branch. Those names change with every build (SvelteKit's default `version.name` is the build time), and that alone moves these entries by up to ±40 B between two builds of the same commit. On `main`, Alert measured 45,993 B, 7 B under its limit, so a build of any branch can fail it by chance. The fix belongs to the runner: set a stable `kit.version.name`, or give these entries headroom.
- **What a new entry costs from now on:** ≈ 25 B for its title, plus ≈ 40–60 B for the entry with its icon and body loaders, on every Settings first view. The cost note at the top of `shell/screens.ts` and the size-limit comment record this.
