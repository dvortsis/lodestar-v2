# Dashboard slots: sessions above a lowered limit (follow-up to #416)

Supersedes one line of `2026-11-04-dashboard-slots.md`, which said an entry above the limit is reported and not drawn.

- **A slot above `limits.phase_total` is drawn as "finishing", not hidden and not warned about.** The runner lowers the limit on a schedule (owner §2.37) while running sessions finish normally, so the list can briefly hold more entries than the limit. Hiding those cards would hide real work.
- **Only a slot number beyond 64 is reported and not drawn,** so a typo cannot cost memory or break the other tables. Rows 1 to the limit are always drawn, idle if unlisted; finishing rows follow in slot order.
- **No owner number or security decision changes.**
