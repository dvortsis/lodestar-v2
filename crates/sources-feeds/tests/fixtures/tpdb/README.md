# ThePornDB fixtures (synthetic, from public docs)

These answers are **synthetic, built from ThePornDB's public API documentation and schema examples**, not recorded from the live service. Recording needs an account, an API key and network access the ENR-2b session didn't have; real recordings, with keys scrubbed, come with the plugin slice (ENR-4).

Every name, id, title, studio and URL is made up (`Example Performer One`, `00000000-0000-4000-8000-…`, `*.example`). None is a real person, studio or scene.

| File | Shape | What it exercises |
|---|---|---|
| `scenes_search.json` | `GET /scenes?parse=…`: `{"data": [...], "links", "meta"}` | Two good items (a `Scene` with three performers, a `JAV` with nulls), one without an `id`, one with a bad `id`, one that isn't an object; personal fields (`extra`, `parent.extras`, `bio`, images) marked `SENTINEL-…` that must never reach the output |
| `scene_hash.json` | `GET /scenes/hash/{hash}`: `{"data": {...}}` | A single lookup; its performer id (an integer) is never kept |
| `movie.json` | `GET /movies/{id}` | `type: "Movie"` and an unread nested `scenes` list |
| `miss.json` | a lookup that found nothing | The error-message answer: no match, no stop |
| `empty_search.json` | a search with no results | `data: []`, `meta.total: 0` |
