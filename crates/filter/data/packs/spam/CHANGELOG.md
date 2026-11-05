# Spam pack changelog

One entry per rule version, newest first. Each names its direction against the version before (`widens`, `narrows`, `action-heavier`, `action-lighter` or `mixed`); a first version widens. A release that changes a rule adds an entry here and bumps the version in `crates/filter/src/packs/manifest.rs`.

## SP-3@1

First version, `widens` (against nothing). Matches flooding: 20 or more near-duplicate names within an hour from one publisher (as one source names it), or, from a live feed (Nyaa, Sukebei) that names no publisher, from that feed. Other sources count only with a publisher named: a full scrape, a download client, `arr`, Internet Archive, Academic Torrents, Prowlarr, the torrent cache and importers can send a backlog. The DHT and enrichment sources never count. A near-duplicate has the same title once episode, quality and group are left out. Judged when a torrent is stored, in order, never before. Added after RP-0 (RP-4), so a store seeded before it has it pending, in shadow, until it's accepted. Action: flag from 20, demote from 100.

## SP-1@1 (baseline)

First version, `widens` (against nothing). Matches a site advert before or after the title (`www.site.com - Title`, `[site.org]`). Action: demote.

## SP-2@1 (baseline)

First version, `widens` (against nothing). Matches a keyword-stuffed name over 300 characters, except music. Action: hide.
