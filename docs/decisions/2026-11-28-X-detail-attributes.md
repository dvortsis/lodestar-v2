# X-detail-attributes decisions (PR pending)

- **`GET /torrents/{hash}` carries `attributes: {[key: string]: string} | null`, required in the schema** (runner, step 0, 2026-11-28). It holds the release attributes the name parsed to. It's the map `POST /compare` shows for the same torrent, so one torrent reads the same in the detail view, Compare and a search hit (§10.5, §10.6).
  - **One definition.** `torrents::release_attributes(&Record)` builds the map for both routes, and a route test checks that compare's column (its nulls left out) equals the detail's map. Each key appears once. Its values are joined with `", "` in the record's order: `alt` is stored once per alternative title, and list fields are already sorted and comma-joined by `Release::fields`. JSON key order carries no meaning (both maps are sorted by key).
  - **Null and `{}` differ.** `null` means there's no enrichment record yet (not classified), or the record is one this build can't read. `{}` means it was classified and the name gave nothing. Only the keys the torrent has are listed; a value is never null or empty.
  - **`res` is sent as stored**, as compare sends it. `SearchHit.resolution`'s enum filter (X-hit-resolution) stays on the hit, where the web client narrows on it. A detail consumer that needs the canonical set reads `Resolution::ALL`'s values below.
- **No parse at request time and no new column.** The parser runs when a torrent is classified, and its result is stored in the enrichment record (`lodestar_classify::Record.release`), which compare and search already read. The route adds one `store.enrichment(id)` point read and a `Record::decode`, under the reader lock it already holds.
  - Measured in a release build: 1.1–1.4 µs to decode and build the map, and at most 1.3 µs for hostile names. Neither `docs/metrics.md` nor §3 gives the detail route a budget, and this is negligible beside its swarm, claims and catalogue reads.
  - The parse at classify time costs 22–94 µs on hostile names. Input is bounded twice: metainfo caps a name at 4,096 B (`Limits::max_name_bytes`), and the parser cuts it to `MAX_INPUT_BYTES` (512 B).
- **Same visibility as the rest of the body.** An Adult torrent while "Show adult content" is off is still the 404 `adult_hidden`, before anything is read. The `title` attribute is the record's effective title, as search and compare show it. When a plugin-resolved title exists (`resolved_via: plugin`, which ENR-10's lookups never produce today), it isn't gated by the source terms that `identified_as` applies. That's the same as search and compare, so a follow-up that resolves titles via plugins must gate all three together.
- **UI not in this slice** (runner, 2026-11-28). Showing these needs a human label per key and en/zh strings, which is design work. The runner queues X-detail-attributes-ui. The keys it starts from, in `Release::fields` order, and their values:

| Key | Values |
|---|---|
| `title` | The title as written, dots and underscores turned into spaces |
| `alt` | Other titles, comma-joined |
| `year` | A year or span: `2008`, `1990-1999` |
| `season`, `episode`, `absolute` | A number or span: `2`, `1-10` |
| `date` | `YYYY-MM-DD` |
| `res` | `480p` `576p` `720p` `1080i` `1080p` `1440p` `2160p` `4320p` |
| `source` | `cam` `telesync` `telecine` `screener` `workprint` `vhs` `tv` `dvd` `hdrip` `bluray` `remux` `web-dl` `webrip` |
| `service` | A streaming-service code (`amzn`, `nf`, …) |
| `vcodec` | `h264` `h265` `av1` `vp9` `xvid` `divx` `mpeg2` `vc1` |
| `bits` | Bit depth: `10` for video, `16` or `24` for music |
| `hdr` | Comma-joined from `hdr` `hdr10` `hdr10+` `dv` `hlg` |
| `acodec` | Comma-joined from `aac` `ac3` `eac3` `dts` `dts-hd` `dts-hd-ma` `dts-x` `truehd` `atmos` `flac` `mp3` `opus` `lpcm` `alac` `vorbis` |
| `channels` | `5.1`, `2.0`, … |
| `lang`, `subs` | Comma-joined ISO 639-1 codes, plus `multi` |
| `edition` | Comma-joined from `extended` `directors-cut` `unrated` `uncut` `uncensored` `remastered` `theatrical` `imax` `criterion` `special` `collectors` `anniversary` `ultimate` `final-cut` `open-matte` `deluxe` `goty` `definitive` |
| `flags` | Comma-joined from `proper` `repack` `real` `internal` `limited` `3d` `dubbed` `hardsub` `retail` `portable` `cracked` `hybrid` |
| `pack` | `yes` (a season pack, complete series, collection or discography) |
| `group` | The release group as written |
| `container` | A file format from the extension or a format word (`mkv`, `epub`, `nsp`) |
| `artist`, `album`, `author` | As written |
| `bitrate` | `320kbps`, or a LAME VBR preset `v0` |
| `sample` | A sample rate, as `Release::fields` renders it |
| `version` | As written |
| `platform`, `arch` | Comma-joined platform and architecture codes |
