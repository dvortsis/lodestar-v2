# X-fleet-guard-docs (docs; follow-up to #852 plus two logged follow-ups)

The runner decided this slice.

## Do
1. **`docs/operations/cloud-fleet-runbook.md` §3.7 (from #852's delta review):**
   - list the two new parts, `damaged not reported` and `host_guard_drops not reported`;
   - fix "ends with `not clean: …`": `; host_guard_drops <n>` and `; host_guard_drops not reported` can follow it. Say "contains", or list the order.
2. **The drill around line 296 of the same runbook** quotes the copy-off result without the guard state. Bring it in line with §3.7.
3. **Follow-up `docs-host-icmp-note-user-setup`.** In the user's gluetun guide (find it under `docs/user/`; `git grep -il gluetun docs/user`), add a short note for people running their own gluetun setup. Explain what host ICMP (ping) does while the tunnel is down, as the fleet's host guard handles it (see `docs/decisions/2026-11-13-X-gluetun-guard-reboot.md` and the X-gluetun-host-guard decisions). Recommend the safe choice.
   - Follow `.claude/skills/user-docs/SKILL.md`: plain, defined terms, no addresses, no IDs.
   - If the guide has no natural home for it, say so in the PR and put it in the troubleshooting section.

## Owns
`docs/operations/cloud-fleet-runbook.md`, the gluetun page under `docs/user/`, a line in `docs/decisions/2026-11-13-X-scrub-guard-fields.md`, and your own placement line in `runner/dashboard.yaml`.

## Rules
- Docs only: no code changes.
- Keep it ≲ 80 lines.
- Run the repo's doc checks if any apply to `docs/user/` (see the user-docs skill's gates).
- Follow the steward skill, and send "ready <PR> <sha>".
