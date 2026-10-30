# Metainfo golden corpus

Each `<case>.info` is a raw bencoded info dictionary, exactly as BEP 9 would deliver it. `<case>.expected` is what `Info::parse` makes of it, rendered by `tests/golden.rs`: the version, encodings, sizes, warnings and one line per file (decoded path, size, attributes, symlink target, whether it has a `pieces root`).

The `aN-` cases are the inputs from the v0.5 parser review (area 1, findings 1–4); see `docs/decisions/2026-10-30-F-metainfo.md`.

To add a case, drop in the `.info` file and run:

```sh
UPDATE_GOLDEN=1 cargo test -p lodestar-metainfo --test golden
```

Then read the new `.expected` file before committing it: it records what the parser *should* do, not just what it does.
