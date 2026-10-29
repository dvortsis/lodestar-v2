# Settings preparation decisions (local, unpublished)

- **Separate product worktree.** DUR-13–15 source stays outside the baseline
  integration worktree and frozen artifacts. This follows the approved next-soak
  backlog and permits independent validation and review.
- **Existing form engine.** Generated core screens use its atomic multi-section
  save, revision checks, preview, conflict handling, cancellation and leave guard
  (architecture §10.10.5). Screen lifetime owns engine destruction.
- **Guarded capabilities remain unavailable.** Network values can be inspected,
  but edits await §10.10.6 reachability and address recovery. Other fields requiring
  privacy, destructive or security confirmation are locked until their dedicated
  interactions exist. This is an interim source-preparation boundary, not a change
  to the approved final UI.
- **Unknown status stays explicit.** The interim overview does not report a healthy
  installation without system/network evidence. Section-name filtering is named
  separately from the promised complete field search (§10.10.3).
- **Restart state is derived.** A section's saved restart requirements must survive
  opening the page and remote refresh. Deriving them from current section values
  replaces save-only assignments and avoids stale status.
- **Acceptance budgets unchanged.** Existing size gates remain blocking. This slice
  adds the approved 45KB gates for the newly implemented Settings routes; current
  failures are recorded rather than reclassified as accepted.
- **Bundle measurements fail closed.** Route accounting includes SvelteKit's root
  error node, nested layouts/errors and server-load indices. Missing build inputs,
  manifest dependencies or the implemented Settings controls family are errors.
  Measurements made with the older resolver are historical, not directly
  comparable evidence of an optimization. Entry-aware Settings grouping was
  rejected because it increased loaded bytes and hid the controls boundary.
- **Keep setup stable through a cold font load.** Shorter introductory/help copy
  preserves the token format, startup-log source and whole-line paste guidance.
  Deterministic held-font fixtures cover desktop and narrow layouts; the existing
  CLS threshold, font policy and responsive form sizing remain unchanged.
- **Export follows the existing backend contract.** General downloads
  `GET /settings/export` with an explicit preferences option. The backend owns
  redaction and portable TOML serialization. Its tables follow settings sections,
  not `config.toml` mappings (`settings/portable.rs`); the UI states that distinction
  and does not present this as the planned per-screen config view or a library
  backup. No import, backup scheduler, reset-all or repair is implied.
- **Export lifetime belongs to the screen.** Cancellation and unmount abort the
  request and discard late responses. Unexpected content types cannot become
  downloads; authentication errors use the shared session handler. Only the latest
  download object URL is retained, released on replacement or destruction. Error
  feedback never renders raw response text or filesystem paths.
