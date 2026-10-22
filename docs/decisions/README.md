# Decisions

Every decision that changes or clarifies the architecture is recorded here, one file per slice or PR. This replaces appending to `docs/architecture.md` §16 (owner, 2026-10-22: one shared section made almost every PR conflict with every other). §16 keeps everything recorded before then. Read it together with this folder.

## Adding an entry
- **One new file per PR:** `docs/decisions/<YYYY-MM-DD>-<slice-id>.md`, for example `2026-10-23-B5c.md`. A second PR for the same slice adds a suffix (`-r2`, or a short topic).
- **Never edit another slice's file.** To change an earlier decision, add a new file that says which file it supersedes and why.
- **Start with a heading** naming the slice and the PR: `# B5c decisions (#91)`. Then use the same style §16 used: short bold-led bullets, each with its reason, citing the architecture sections it affects.
- If the decision touches one of the owner's decisions (`docs/owner-decisions.md`), stop and ask the owner first, as before.

## Reading them
Files sort by date. The newest file about a topic wins over older ones and over §16.
