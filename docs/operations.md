# Lodestar operations guide: running and tuning a node

This guide covers what an operator needs to know to run a node and adjust it. `docs/architecture.md` stays the source of truth for design and decisions. This file says what the settings do and what to expect.

## Swarm health

Swarm health is how Lodestar learns whether a torrent is alive: how many seeders and leechers it has, how many times it has completed, and how many trackers know about it. The numbers drive the seeders and trackers filters, the seeders and trackers sorts, and the ranking penalty for suspect swarms (architecture §3, §6, §15).

### What scraping is, and what it isn't

- A scrape asks a tracker for the seeders, leechers and completed counts of up to 74 hashes at a time (fewer over HTTP).
- A scrape announces nothing. Our IP address never enters a swarm's peer list, so nobody downloading that torrent sees us.
- The tracker does learn which hashes we asked about. That's the cost of scraping, and it's why private torrents are never scraped.
- Torrents flagged `private`, and keys that only private sources reported (`private_source`), are never scraped, by tracker or by DHT. A public lookup would leak the hash (BEP 27, architecture §18.7).
- Lodestar keeps counts only. It never keeps peer addresses or the bloom filters a DHT scrape returns (architecture §18.7).

### Reading the numbers

- **Seeders and leechers** come from the trackers that answered, and from the DHT estimate where there is one. The fake-seeding check below can discount a source.
- **"—" means never scraped.** It isn't zero. A real `0` means a tracker or the DHT answered and said there is nobody.
- **"Trackers reporting" and "checked":**
  - The detail view shows "n of m trackers", where m is how many trackers were asked about this hash and n is how many knew it.
  - Only a tracker that answers with seeders + leechers + completed > 0 counts as reporting.
  - The reason is that opentracker-style trackers answer with zeros for hashes they've never seen. Counting those would make every torrent look known.
  - So "reporting" is a sampled signal that ages. The detail view shows when it was last refreshed.
- **"≈"** marks a DHT estimate (see BEP 33 below).

### The `[scrape]` config section

All keys are optional. This example shows the defaults:

```toml
[scrape]
enabled = true

# Public UDP trackers to scrape. This is the built-in default list.
trackers = [
  "udp://tracker.opentrackr.org:1337/announce",
  "udp://open.demonii.com:1337/announce",
  "udp://open.stealth.si:80/announce",
  "udp://tracker.torrent.eu.org:451/announce",
  "udp://exodus.desync.com:6969/announce",
  "udp://explodie.org:6969/announce",
  "udp://tracker.theoks.net:6969/announce",
  "udp://opentracker.io:6969/announce",
  "udp://tracker.dler.org:6969/announce",
  "udp://tracker.tiny-vps.com:6969/announce",
]

# BEP 33 estimates from the DHT, for new, viewed and rediscovered torrents.
dht = true

# Seconds between UDP requests to each tracker. Unset follows the DHT
# intensity: Low 5, Balanced 2, Aggressive 1.
# interval_secs = 2

# Seconds between HTTP requests to each tracker. Unset is 3x the UDP interval.
# http_interval_secs = 6
```

- `enabled = false` turns off all scraping, tracker and DHT. The environment variable `LODESTAR_SCRAPE_ENABLED` overrides it.
- `dht = false` keeps tracker scraping and drops the BEP 33 estimates.
- `trackers` accepts UDP (BEP 15) and HTTP or HTTPS (BEP 48) URLs.
  - Unknown keys in `[scrape]` are startup errors, so typos surface.
  - So is a tracker URL that can't be scraped, such as `wss://` (WebTorrent) or an HTTP URL whose last path segment doesn't start with `announce`.
  - The same tracker listed twice counts once. Lodestar compares the canonical form: `udp://host:port` for UDP, with any BEP 41 path dropped, and the normalised URL without its fragment for HTTP.
- `interval_secs` and `http_interval_secs` are per tracker. A tracker's own `min_request_interval` can only slow it down further. It never speeds it up (see tracker health below).
- Scrape errors never include the tracker URL, because it may hold a passkey.

### The schedule

Every torrent that can be scraped is in one of a few lanes. The scheduler serves them in turn (deficit round robin), so a flood in one lane can't starve the others.

- **New torrents** are scraped once, soon after they're stored.
- **Viewed and top-of-search torrents** are scraped promptly. A torrent you open is refreshed if its data is older than 30 minutes.
- **Rediscovered torrents** get priority. When a known hash shows up in the DHT again, that's a free liveness signal (architecture §5), so its scrape moves up.
- **Alive swarms** (some seeder or leecher) are rescraped every 6 hours. While the counts stay steady, the gap doubles, up to 3 days. It resets to 6 hours when a count moves by a fifth, or by 2 peers or more.
- **Dead swarms** (nothing reported) are rescraped every 2 days, doubling to 30 days.
- **Unreachable rounds**, where no tracker and no DHT answer arrived, retry after 1 hour, doubling to 1 day.
- Every interval has ±10% jitter, so a batch of torrents stored together doesn't come due together.

### Throughput at Balanced

- Each tracker gets one UDP request every 2 seconds. A request carries up to 74 hashes.
- That's 37 torrents a second per tracker, or about 3.2 million torrent-scrapes a day (37 × 86,400).
- The limit is per tracker, so adding trackers adds coverage ("n of m trackers") but not more distinct torrents per second.
- A full sweep of 10 million hashes against 20 trackers would take about 2.7 million UDP requests per pass. Trackers ban clients that do that, so Lodestar doesn't try (architecture §15). The schedule above spends the budget where it matters: new, viewed and rediscovered torrents first, then a slow decay over the rest.
- Low (one request every 5 s) and Aggressive (one every 1 s) scale this by 0.4 and 2.

### Tracker health

Each tracker is in one of four states:

| State | Meaning | What Lodestar does |
|---|---|---|
| Ok | answering | scrapes at the configured interval |
| Backoff | a recent failure (timeout or error) | waits 1 minute, doubling on each further failure |
| Down | 5 failures in a row | skips the tracker, except for a probe once an hour |
| Banned | a refusal that mentions a rate limit or ban, or HTTP 429 or 403 | stays away for 24 hours, doubling on repeat to a week |

- A tracker's `min_request_interval` (the HTTP scrape response may carry one) only ever slows requests. It's honoured up to a day.
- Health isn't saved. In M4 every tracker starts as Ok after a restart.
- The status page lists each tracker with its state, so a banned tracker is visible rather than silently missing.

### Fake seeding

Some trackers inflate their counts. Lodestar checks swarms against each other:

- A swarm is flagged **suspect** when one tracker claims at least 1,000 seeders and at least 50 times more than every other source. The other sources are the other trackers and, if two or more DHT nodes answered, the DHT estimate.
- For a suspect swarm, the counts come from the other sources, and the detail view says why.
- It is a ranking penalty, not a deletion. The torrent stays searchable.
- It's kept apart from the stored junk score, because swarms change.

### Routing, proxies and VPNs

- Scrapes leave from the same bind address as the DHT (`dht.bind`). If the crawler runs through a VPN, so do the scrapes.
- HTTP scrapes ignore `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY`. They go out the bound interface like everything else the crawler does.
- Trackers see the IP the scrape leaves from. On a home connection that's your own address, and public trackers may log it. A VPN is advised, and in Docker `network_mode: service:gluetun` covers the scraper too.
- If the status page shows trackers as banned, lengthen `interval_secs`.

### BEP 33: the DHT estimate

- A DHT scrape asks nodes near the hash for their seed and peer bloom filters, merges the filters of the 8 closest nodes that have any, and estimates the counts from the merged filter.
- It's an estimate, so it's shown as "≈". It is accurate for small swarms and saturates near 7,800.
- Zero responders means no node near the hash stores it. That reads as "unknown", not "dead".
- Only the two counts are kept. The filters, and any peers learned along the way, are dropped when the lookup ends.
- DHT scrapes are limited to about 10% of the query budget, so they serve new, viewed and rediscovered torrents but not the decay schedule. They don't feed the metadata fetcher.

### Hot columns

- Seeders, leechers, trackers, the DHT estimate and last seen live in in-memory arrays, so searches can filter and sort on them with no locks. They cost about 20 bytes per torrent of RAM, about 200 MB at 10 million torrents, which counts against the 1 GB budget.
- On a clean shutdown they're snapshotted to `data/hot/*.col` with a stamp.
- After a crash, or if the stamp doesn't match the database, they're rebuilt from SQLite at startup. That takes longer on big corpora, so expect a slower start.
- SQLite stays the source of truth. Nothing is lost if the snapshot is deleted.

### Watching it

- The status page has a "Swarm health" section: scraped, alive and suspect counts and a table of trackers with their state, counts and last answer. It's hidden when scraping is off.
- Prometheus metrics cover scrape requests by tracker and outcome and the DHT scrape counters. Their names arrive with the binary wiring, so check `/metrics` on your build.
