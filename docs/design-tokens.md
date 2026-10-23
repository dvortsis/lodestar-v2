# Design tokens

The contract between the theme compiler (`web/scripts/themes.ts`, W1), the
static token stylesheet (W2), the shared primitives and every screen. It
fixes **every CSS custom property the UI may use**: its exact name, its role
and where its value comes from. Architecture §10.2 (foundations) and §10.18
(colour) are the source of truth. If this file and the architecture document
disagree, the architecture document wins and this file is fixed.

## The rule

- **Components use tokens only.** No hex, `rgb()`, `hsl()` or `oklch()`
  literals in `.svelte` files or feature CSS, and no arbitrary Tailwind values
  (`p-[13px]`, `text-[#fff]`, `w-[312px]`, `duration-[180ms]`). W0's lint
  rule enforces both.
- **Use the Tailwind/daisyUI utility a token generates** (`bg-base-100`,
  `text-muted-text`, `rounded-box`, `p-4`, `text-body`) where one exists.
  Use `var(--token)` directly only where no utility exists (SVG `fill`/`stroke`
  in charts, `style:` bindings, keyframes).
- **No token outside this file.** A new token is a change to this document,
  reviewed in the main session, before any component uses it.
- **Geometry is defined once** (§10.2, `component-geometry`): radii, heights
  and borders come from these tokens or the canonical primitive, never a
  recipe repeated in page code.
- The brand values (`background`, `surface`, `text`, `primary`, `accent`,
  6 `categorical`) are **compiler inputs, not tokens**. Components never read
  them; they read the derived role tokens below. The single exception is the
  `--brand-*` group, for the compass mark only.

### Where values come from

| Source | Meaning |
|---|---|
| **compiler** | Emitted per theme by `web/scripts/themes.ts` from the theme JSON (§10.18). Riverstone light/dark go into critical CSS; the other 46 themes into lazy `themes.json`. Values change with the theme. |
| **static** | Defined once in the app stylesheet's `@theme` / `:root` block (W2). Same in every theme. |
| **daisyUI** | A daisyUI 5 theme variable. The compiler (colours) or the static sheet (shape) sets it; daisyUI's component CSS reads it. |

Values in the tables are **Riverstone light / Riverstone dark** where §10.18
gives them. "rule" means §10.18 gives a derivation rule but no Riverstone
value; the compiler computes it and W1's golden test pins the result.

---

## 1. Colour: daisyUI 5 theme variables (compiler)

daisyUI 5 names its colours `--color-*`, and Tailwind v4 turns each into
utilities (`bg-primary`, `text-base-content`, `border-base-300`). Mapping from
§10.18 ("daisyUI 5 mapping").

| Token | Role | Riverstone light | Riverstone dark |
|---|---|---|---|
| `--color-base-100` | Surface: panels, rows, the drawer, dialogs | `#FFFFFF` | `#22313A` |
| `--color-base-200` | Background: the page | `#F3F6F5` | `#141F25` |
| `--color-base-300` | Divider: decorative separation only; never the only cue; no contrast minimum. **= `divider`** (§10.18, brand `border`) | `#DCE4E1` | `#354851` |
| `--color-base-content` | Text on base-100/200 (≥ 4.5:1 on both; High Contrast ≥ 7:1) | `#243C44` | `#E5EEED` |
| `--color-primary` | **= `primary-ink`**: links, primary buttons, focus ring (≥ 4.5:1 on bg and surface) | `#4A7789` | `#85AFC0` |
| `--color-primary-content` | Text on `primary`: the higher-contrast of surface/background | `#FFFFFF` | `#141F25` |
| `--color-secondary` | Not a Lodestar role. Emitted equal to `--color-primary` so daisyUI classes stay valid; **`*-secondary` classes are banned by lint** | = primary | = primary |
| `--color-secondary-content` | As above | = primary-content | = primary-content |
| `--color-accent` | **Accent fill**: selected chips, tints. Never text or a boundary in light mode (2.65–2.89:1) | `#829F8A` | `#A0C3A7` |
| `--color-accent-content` | Label on an accent fill: light uses `text`, dark uses `background` (≥ 4.5:1, or large/bold) | `#243C44` | `#141F25` |
| `--color-neutral` | Neutral fill (the `#8C9BA7` family): neutral badges, Kbd background. Dark is §10.18's `#8C9BA7`; **light is a C0 choice** (the light slate data variant, so it keeps ≥ 3:1 on surface) | `#8796A2` (C0 choice) | `#8C9BA7` |
| `--color-neutral-content` | Label on neutral: whichever of `base-content`/`base-100` passes 4.5:1 on it (rule) | rule | rule |
| `--color-info` | Info status: icon + text, always with a label. **= `primary-ink`** light, **= `primary`** dark | `#4A7789` | `#85AFC0` |
| `--color-info-content` | Text on an info fill: the higher-contrast of surface/background (rule) | rule | rule |
| `--color-success` | Success status: **= `accent-ink`** light | `#5B7763` | `#A0C3A7` |
| `--color-success-content` | Text on a success fill (rule as info) | rule | rule |
| `--color-warning` | Warning status (from ochre `#C4A765`), ≥ 4.5:1 | `#896D2A` | `#C4A765` |
| `--color-warning-content` | Text on a warning fill (rule as info) | rule | rule |
| `--color-error` | Error status (terracotta shifted ~12° redder, more chroma); distinct from the Software data colour | `#B45253` | `#DC8080` |
| `--color-error-content` | Text on an error fill (rule as info) | rule | rule |

Status colours are **never the only cue**: every status shows an icon and a
text label (§10.2 "Colour use").

## 2. Colour: Lodestar role tokens (compiler)

Roles daisyUI has no slot for. (`primary-ink` has one: it *is* `--color-primary`, used both as the button fill and as link/focus-ring ink, so it is not emitted twice.) Named with the `--color-` prefix so Tailwind v4
generates utilities (`text-muted-text`, `border-field-border`, `bg-hover`).
Rules from the §10.18 "Derived token" table.

| Token | Role | Riverstone light | Riverstone dark | Gate |
|---|---|---|---|---|
| `--color-accent-ink` | Accent hue as text/icon | `#5B7763` | `#A0C3A7` | ≥ 4.5:1 |
| `--color-muted-text` | Meta, captions, secondary labels | `#567079` | `#B2C3C5` | ≥ 4.5:1 on bg and surface |
| `--color-field-border` | Input borders and any **essential** boundary (checkbox, toggle track, table-mode cell edge that carries meaning) | `#8796A2` | `#8C9BA7` | ≥ 3:1 on surface |
| `--color-hover` | Hover tint on rows, menu items, cards | `#F1F5F6` | `#2A3B45` | text on it ≥ 4.5:1 |
| `--color-selected` | Selected/active row and item tint | `#E7EDF0` | `#30434D` | text on it ≥ 4.5:1 |

- `divider` from §10.18 is **`--color-base-300`** (§1), not a second token.
- `info`/`success`/`warning`/`error` from §10.18 are the daisyUI tokens in §1.
- The brand `muted` (`#61787D`, 4.30:1 on background) is not emitted: one
  muted-text token serves both surfaces (§10.18).
- **Focus ring:** `outline: var(--focus-ring-width) solid var(--color-primary);
  outline-offset: var(--focus-ring-offset)` (§6).

## 3. Colour: data marks (compiler)

Charts, the composition bar, facet swatches and legends read these. The
**slot number is the category group** from §10.18's category → colour table,
identical everywhere (composition bar, facets, legends, charts). Values are
the *data variants* (≥ 3:1 on surface); dark uses the supplied categorical
values.

| Token | Group (file categories) | Riverstone light | Riverstone dark |
|---|---|---|---|
| `--data-1` | Video | `#527F91` | `#527F91` |
| `--data-2` | Audio | `#AE9150` | `#C4A765` |
| `--data-3` | Software & executables | `#BF8173` | `#BF8173` |
| `--data-4` | Documents (document, ebook, subtitle) | `#8796A2` | `#8C9BA7` |
| `--data-5` | Images | `#7E9B86` | `#829F8A` |
| `--data-6` | Archives & disc images | `#9381B6` | `#9381B6` |
| `--data-other` | Other (= `muted-text`) | `#567079` | `#B2C3C5` |

- **Theme JSON → slot:** a theme's `categorical[i]` (Riverstone order: blue,
  sage, ochre, terracotta, slate, plum) feeds slot
  `[1, 5, 2, 3, 4, 6][i]`. The "data marks (light)" row in §10.18 is listed in
  categorical order; the slot table is authoritative for `--data-N`.
- **Fixed segment order** in the composition bar: Video, Audio, Images,
  Documents, Archives, Software, Other (§10.18) — i.e. `--data-1, -2, -5, -4,
  -6, -3, -other`.
- Segments are separated by a `--compbar-gap` gap filled with
  `--color-base-100` (§7).
- A series that isn't a category (trend lines, funnel bars, KPI sparklines)
  uses `--data-1` for the primary series and `--color-muted-text` for the
  comparison period. Comparison bands use `--color-selected`.
- **Mapping file category → group** (codes from `crates/classify/src/ext.rs`):
  video → 1; audio → 2; software → 3; document, ebook, subtitle → 4;
  image → 5; archive, disc → 6; other → other.

## 4. Colour: brand (compiler, compass mark only)

The compass SVG's fills read these (§10.19), with the Riverstone values as
`var()` fallbacks inside the SVG. **Brand-only: never a UI state, status or
data colour.** `--brand-star` in particular is never used for meaning, so it
can't be confused with Audio ochre or warnings.

| Token | Role | Riverstone light | Riverstone dark |
|---|---|---|---|
| `--brand-surface` | Compass disc/background | `#FFFFFF` | `#22313A` |
| `--brand-primary` | Compass body (brand `primary`, not the ink) | `#527F91` | `#85AFC0` |
| `--brand-accent` | Compass accent | `#829F8A` | `#A0C3A7` |
| `--brand-star` | The star | `#D49A4A` | `#E0A95F` |

Non-Riverstone themes: `--brand-surface` = that theme's surface, `-primary`
and `-accent` = that theme's brand primary/accent. **`--brand-star` keeps the
Riverstone value in every theme** (`#D49A4A` in light mode, `#E0A95F` in dark
mode), because it's the logo; the compiler never derives it.

---

## 5. Spacing (static)

The `layout-spacing` ramp, §10.2. **Tailwind v4's default spacing is kept:**
`--spacing: 0.25rem` (4 px), so a utility step `n` is `n × 4px` and `p-4` is
16 px, exactly as in standard Tailwind. No `--spacing-*` tokens are added and
the scale isn't redefined, so a helper writing ordinary Tailwind gets the
spacing it expects.

**Only these steps are allowed.** The §10.2 ramp in px maps to these utility
steps; any other step (`p-11`, `gap-15`, `m-3.5`, `p-16`, …) is off the ramp.
W2 extends W0's ban check (hex literals and arbitrary values) to reject
off-ramp steps in every spacing utility (`p*`, `m*`, `gap*`, `space-*`,
`inset*`, `top/right/bottom/left`, `translate-*`, `scroll-m*`, `scroll-p*`).

| px | Utility step | | px | Utility step |
|---|---|---|---|---|
| 0 | `0` | | 24 | `6` |
| 2 | `0.5` | | 28 | `7` |
| 4 | `1` | | 32 | `8` |
| 6 | `1.5` | | 36 | `9` |
| 8 | `2` | | 40 | `10` |
| 10 | `2.5` | | 48 | `12` |
| 12 | `3` | | 52 | `13` |
| 16 | `4` | | 56 | `14` |
| 20 | `5` | | | |

In CSS outside utilities, write `calc(var(--spacing) * n)` with an allowed
`n`; never a raw px value.

Usage (§10.2): inside a component 4–12; repeated patterns (row internals,
facet groups) 8–16; between layout regions 24–48. 2, 6 and 10 are optical
exceptions (icons, fine alignment). Density modes change component spacing
only, never the ramp.

## 6. Geometry (static / daisyUI)

### Radii (§10.2)

| Token | Value | Use | Utility |
|---|---|---|---|
| `--radius-sheet` | 16px | Dialogs, sheets, the drawer's leading edge (the single override) | `rounded-sheet` |
| `--radius-box` | 12px | Cards, banners, toasts, panels, illustration frames (daisyUI) | `rounded-box` |
| `--radius-field` | 8px | Buttons, inputs, the query bar, segmented controls (daisyUI) | `rounded-field` |
| `--radius-selector` | 4px | Tags, badges, chips, composition-bar end caps, checkboxes (daisyUI) | `rounded-selector` |

No `rounded-full` on containers or primary buttons. Tailwind's default
`--radius-*` scale is reset so only these four exist.

### Borders and surface effects (daisyUI)

| Token | Value | Role |
|---|---|---|
| `--border` | 1px | The only border width. Containers use `--color-base-300`; fields and essential boundaries use `--color-field-border`. Only where whitespace isn't enough. |
| `--depth` | 0 | daisyUI depth effect off (`minimalist-visual-style`: no shadows-as-decoration, no gradients). |
| `--noise` | 0 | daisyUI noise texture off. |

### Control and target sizes

| Token | Value | Role |
|---|---|---|
| `--size-field` | 0.25rem | daisyUI field base unit. daisyUI's `md` controls are 10 units = **40px** (default), `sm` are 8 units = **32px** (dense toolbars and facets). W2 verifies these heights against daisyUI 5. |
| `--size-selector` | 0.25rem | daisyUI selector base unit (checkbox, radio, toggle). |
| `--control-height` | 40px | Default control height, for non-daisyUI controls (Bits UI triggers, segmented control). |
| `--control-height-dense` | 32px | Dense toolbar/facet control height. |
| `--query-bar-height` | 48px | The query bar (§10.4). |
| `--target-min` | 44px | Minimum hit target under `@media (pointer: coarse)`: every target grows to ≥ 44 × 44. |
| `--icon-size-row` | 16px | Icons in rows. |
| `--icon-size-nav` | 20px | Icons in the toolbar and navigation. |
| `--focus-ring-width` | 2px | Focus outline width (colour `--color-primary`, ≥ 3:1). **C0 choice**; §10.2 gives no width. |
| `--focus-ring-offset` | 2px | Focus outline offset. **C0 choice.** |

## 7. Density and layout (static)

| Token | Value | Role |
|---|---|---|
| `--row-height-compact` | 36px | Compact density: single-line rows. Also the table-mode row height. |
| `--row-height-comfortable` | 64px | Comfortable density (default): two-line rows. |
| `--card-height` | per breakpoint | Cards density: a fixed height per breakpoint, set by the Search slice and recorded here when chosen (not yet specified in §10.2). |
| `--rail-width` | 264px | Facet rail (fixed). |
| `--results-min-width` | 560px | Results column minimum. |
| `--drawer-min-width` | 480px | Persistent drawer minimum (resizable; width persisted). |
| `--drawer-max-width` | 640px | Persistent drawer maximum. |
| `--drawer-overlay-width` | 560px | Drawer overlay at x-large (§10.15). |
| `--drawer-overlay-width-narrow` | 480px | Drawer overlay at large (or full width). |
| `--manuscript-width` | 720px | Settings, help and syntax pages. |
| `--prose-measure` | 70ch | Prose line length cap. |
| `--compbar-min-width` | 64px | Composition bar in a row, minimum. |
| `--compbar-max-width` | 120px | Composition bar in a row, maximum. |
| `--compbar-gap` | 2px | Surface-coloured gap between composition segments. |
| `--toast-width` | 384px | Toast region width from `large` up (full width minus gutters below). The toast fade reuses `--duration-banner` (0 under reduced motion). |
| `--listbox-max-height` | 5 × `--row-height-comfortable` | Popup listboxes (Combobox, later Select/Menu) scroll beyond this. |
| `--art-height-large` | 240px | Illustration height at large (art above text). |
| `--art-height-small` | 160px | Illustration height at medium/small; hidden when the viewport is < 600px tall. |

### Breakpoints (static, Tailwind v4 `--breakpoint-*`)

§10.15 ranges. Tailwind's default breakpoints are reset; the variants are the
range names (`large:`, `x-large:`). Small (320–479) is the base, with no
variant.

| Token | Min width |
|---|---|
| `--breakpoint-medium` | 480px |
| `--breakpoint-large` | 640px |
| `--breakpoint-x-large` | 1024px |
| `--breakpoint-xx-large` | 1366px |
| `--breakpoint-xxx-large` | 1920px |

The illustration hide rule (viewport height < 600px) is a media query, not a
token. Container queries (row, drawer) use these same values.

## 8. Typography (static)

### Families

| Token | Value |
|---|---|
| `--font-sans` | `"Atkinson Hyperlegible Next", "Atkinson Fallback", system-ui, "PingFang SC", "Microsoft YaHei", "Noto Sans CJK SC", sans-serif` |
| `--font-mono` | `"Atkinson Hyperlegible Mono", ui-monospace, monospace` |

The CJK fallbacks are installed family names (what `fc-list` and the OS font
pickers report), because a CSS family name must match one exactly: "Noto Sans
CJK SC" is a real family, bare "Noto Sans CJK" is not. Use the SC, TC, JP or KR
variant that matches the page language. Token names are unchanged.

Mono is for hashes, file paths, query field tokens, the info-dict viewer and
keyboard shortcuts (§10.2). No serif in app chrome.

### Scale (§10.2, 6 steps)

Tailwind v4 text tokens: each step has a size plus `--line-height` (and
`--letter-spacing` where set) sub-tokens, giving `text-caption` … `text-display`.
Tailwind's default `--text-*` steps are reset.

| Token | Size | `…--line-height` | `…--letter-spacing` | Use |
|---|---|---|---|---|
| `--text-caption` | 12px | 16px | — | Badges, meta |
| `--text-small` | 13px | 18px | — | Row metadata, facet labels, raw name (mono) |
| `--text-body` | 14px | 20px | — | Default UI text |
| `--text-body-lg` | 16px | 24px | — | Query input, drawer subtitles |
| `--text-title` | 20px | 26px | — | Drawer and section headings, the drawer title |
| `--text-display` | 28px | 32px | -0.02em | Page headings, KPI values |

### Weights

Only three; Tailwind's other `--font-weight-*` steps are reset.

| Token | Value |
|---|---|
| `--font-weight-normal` | 400 |
| `--font-weight-medium` | 500 |
| `--font-weight-semibold` | 600 |

**Tabular numerals** (`font-variant-numeric: tabular-nums`, utility
`tabular-nums`) for sizes, counts, seeders, trackers and times. That's a
property, not a token.

## 9. Motion (static)

§10.2 (`motion-ui`). Transform and opacity only. Under
`prefers-reduced-motion: reduce`, every duration token is redefined to `0ms`
except `--duration-reduced-fade`, which is the only motion left (opacity
crossfades).

| Token | Value | Use |
|---|---|---|
| `--duration-fast` | 150ms | Hover, focus, press; tooltip/popover **in** |
| `--duration-popover-out` | 100ms | Tooltip/popover **out** |
| `--duration-drawer-in` | 300ms | Drawer/sheet in (transform) |
| `--duration-drawer-out` | 200ms | Drawer/sheet out (transform) |
| `--duration-banner` | 200ms | "N new results" banner |
| `--duration-stagger` | 80ms | Step between items in Discover's first-load reveal (at most 8 items) |
| `--duration-reduced-fade` | 100ms | Maximum crossfade under reduced motion |
| `--ease-out` | `cubic-bezier(0, 0, 0.2, 1)` | Entering. §10.2 says "ease-out"; the curve is a **C0 choice** (Tailwind v4's default value) |
| `--ease-in` | `cubic-bezier(0.4, 0, 1, 1)` | Exiting. §10.2 says "ease-in"; the curve is a **C0 choice** (Tailwind v4's default value) |
| `--scale-press` | 0.98 | Press feedback (`transform: scale(...)`) |

Results and facet updates swap instantly: no token applies. No ambient motion.
`--ease-in-out` and linear easing are not used; Tailwind's `--ease-in-out` is
reset.

---

## 10. §10.18 name → token

Every token named in §10.2 is defined once in §§5–9 above; every §10.18
name maps to exactly one property:

| §10.18 name | Token |
|---|---|
| background | `--color-base-200` |
| surface | `--color-base-100` |
| text | `--color-base-content` |
| `primary-ink` | `--color-primary` |
| `primary-content` | `--color-primary-content` |
| `accent-ink` | `--color-accent-ink` |
| `accent` fill | `--color-accent` (+ `--color-accent-content`) |
| `muted-text` | `--color-muted-text` |
| `field-border` | `--color-field-border` |
| `divider` | `--color-base-300` |
| `brand-star` | `--brand-star` |
| `hover` / `selected` tint | `--color-hover` / `--color-selected` |
| `info` / `success` / `warning` / `error` | `--color-info` / `--color-success` / `--color-warning` / `--color-error` |
| data marks | `--data-1` … `--data-6`, `--data-other` |
| `neutral` | `--color-neutral` |
