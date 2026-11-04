# StashDB fixtures (ENR-3)

**Synthetic, from public docs.** These answers were written by hand in the shapes stash-box's public GraphQL schema describes (`Scene`, `Studio`, `PerformerAppearance`, `Performer`, and GraphQL's own `data`/`errors` envelope). None was recorded from the live service: recording needs an account, an API key and network access the ENR-3 session didn't have. Real recordings, with keys scrubbed, come with the StashDB plugin (ENR-5).

Every name and id is obviously synthetic (`Example Performer One`, `00000000-0000-4000-8000-…`); none belongs to a real performer, studio or scene.

| File | What it is |
|---|---|
| `search.json` | A `searchScene` answer: four kept scenes (one with a performer entry that has no name, which is left out) and three skipped (no id, a bad id, a bad date). |
| `find.json` | A `findScene` answer with one scene. |
| `find_null.json` | A `findScene` answer for no such scene. |
| `errors.json` | A failed query: `errors` and `data: null`. |
| `personal.json` | A scene whose performer carries every personal field stash-box has (birth date, measurements, birthplace, aliases, images and more), each valued `PERSONAL-…` or `999…`, and a performer id. The parser must keep none of them. |

`fuzz/seeds/stashdb/` holds copies of the `.json` files.
