# X-cmp-egress-api decisions

A companion's route in the API, and `PUT /companions/{id}/egress`, which changes the route and restarts the companion (architecture §18.10, §18.7; plan `docs/plan/2026-11-06-v06-companions.md` Q18). This closes the gap CMP-9a found (`2026-11-11-CMP-9a.md`, "Egress"). None of these decisions loosens an owner decision or weakens a security decision. The runner flags the refinements to the owner (§2.19).

- **`Companion` gains `egress` and `runs`. Both are read-only in GET.**
  - `egress` is null for a companion that takes no route (gluetun). Otherwise it is `{from, setting}`, where `setting` is the source route's own type and names (`EgressSetting`: `route`, `proxy`, `dns`), read the way `egress_setting` reads it at start:
    - the companion's own row (`from: saved`);
    - otherwise the route of the source it serves (`from: source`), which it copies as its own at its next start.
  - GET writes nothing: the read is the same function the start path uses (`egress_row`), minus the copy.
  - A row that doesn't decode shows `setting: null`, never direct, because the route stays down.
- **`runs` is CMP-9a's "where it runs", as a value the server supplies.** The UI will no longer need its fixed sentence. It takes one of three values:
  - `lodestar`: on this computer, started and stopped by Lodestar. That means Lodestar installed it, or an install here would put it there.
  - `own_container`: Lodestar runs in a container, so the companion comes from its template.
  - `yours`: you added it, or its own installer is offered for you to run (`guided_only`).
- **The PUT validates the route exactly as a source's route is validated.** The egress part of `check_settings` became `ops::check_egress`, shared by both:
  - `interface` and `bind_address` are refused with `409 route_unavailable`, because their lookups would leave outside them;
  - a direct route with a proxy or a DNS choice is `400 invalid_egress`;
  - a SOCKS5 proxy must pass `parse_proxy`, which refuses the unspecified address, multicast, broadcast, disguised numbers and port 0;
  - the body is `deny_unknown_fields`, so no field can turn a SOCKS5 route's kill switch off or name a fallback (`422 bad_body`).
  - The binary converts the route with the sources' own `egress_back` and saves it only if it reads back as itself, so a start never finds it unreadable.
- **Refinement: a route change needs the admin password.** No other companion write asks for it (install, remove, restart). A source's route change always does, though (`patch_source`: "the route is what keeps a source's traffic on the VPN"), and the brief asks for the same rules as the sources' route. So the password is checked first, before the id or the route, as the reveal does it: `428 confirm_required`, `401 bad_password`, the shared rate limit. Same session guard and same-origin JSON as every companion write.
- **Refusals happen before anything is saved:**
  - `404 unknown_companion`;
  - `409 takes_no_route` (new code with its own sentence: none existed, and `no_route` is already the router's 404);
  - `409 not_installed` (any companion Lodestar didn't install, and every companion inside a container);
  - `409 busy`.
- **`busy` before saving.** A running restart holds only the manager's in-memory slot, not the journal. So the manager gained `Manager::busy(&id)`, a read of that slot list (`crates/companions/src/manager.rs`, outside the brief's owns; named to the runner). A route change holds a lock of its own from that check until its restart has claimed the companion. A second change therefore sees that restart and is refused `busy` with nothing saved, and the route a started restart reads is the one its answer reports.
- **Once the route is saved, the answer is `200` with `applied`.**
  - `applied: true` means the restart through the existing path (`Live::restart`, which is `Manager::restart`) has begun.
  - Otherwise `applied: false`, and `not_applied` carries the restart's refusal code (`not_wired`, `busy` if another operation won the race after the check) or `restart_failed`. The route stays saved and is used at the next start.
  - It never reports applied when the restart didn't begin. "Began" is the same promise `POST …/restart`'s `202` makes; the row's health shows when the companion is back.
- **No secrets.** A companion's route carries no credentials (`2026-11-11-X-cmp-egress-prelaunch.md`, "Credentials"), and the request type has no field for one: a `user:pass@host` proxy is `invalid_egress`, and a `password` or `credentials` field inside `egress` is `bad_body`. Refusals name the field, never the value. The request's `Debug` redacts the admin password.
- **No new log event.** A source's route change logs none either. A `companions.egress_changed` event, with the route kind only, would touch five shared files (`log_events.rs`, `logs_catalog.txt`, `event-names.ts` and the en/zh logging messages). That is left for a follow-up if the runner wants an audit line. The restart's existing events still fire.
- **No UI in this slice.** The route editor on Settings → Companions, and switching the screen's "where it runs" sentence and its route notes to these fields, are a follow-up. The generated `openapi.json` and `schema.d.ts` are the only `web/` changes.
