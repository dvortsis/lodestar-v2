# X-insights-source-names (runner brief, 2026-11-21Z)

Kind: core, model opus. Workstream ws-web (its slices.yaml entry already exists on main; keep its id).
Deps: X-labels (merged). It owns openapi.json: no other open slice may own it while this runs.

## What
The scope is the slices.yaml entry's notes, which this brief doesn't repeat:
- SegmentYield and DiscoverSource gain the plugin's display name, as ProvenanceSource.name does.
- The client drops its own table (format/label-source-names.ts, 287 B) and shows the server's name, falling back to the id.
- The insights export.csv keeps its key column and adds a name column. Say so in the release notes.
- The Insights stage pages size limit comes down when the table goes.

## Step 0 (send to the runner before building)
1. List every DTO and endpoint that gains `name`, and where the server gets the name (the plugin registry). Say what a source with no installed plugin shows.
2. Name every client caller of label-source-names.ts, and say what replaces each one.
3. List the CSV columns before and after, and any golden files that change.
4. Estimate the changed lines.

## Rules
- Regenerate openapi.json and schema.d.ts with the repo's tool. Never hand-edit them.
- A plugin's name is untrusted text: render it as text, never as HTML. Apply the same bounds and invisible-character handling ProvenanceSource.name uses. Run /security-review on the delta.
- Size: the Insights entry comes down to the measured size, rounded up to 0.01 kB. No other entry goes up without its numbers being sent first.
- Unit tests on both sides, plus the CSV golden. No fixed sleeps, and condition waits of at least 10 s.
- Branch slice/X-insights-source-names, PR title "X-insights-source-names: <title>". Push once per round, then send "ready <pr> <sha>" when it's green. Follow .claude/skills/steward/SKILL.md, and never merge.
