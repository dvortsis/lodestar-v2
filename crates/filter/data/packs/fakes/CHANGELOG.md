# Fakes pack changelog

One entry per rule version, newest first. Each names its direction against the version before (`widens`, `narrows`, `action-heavier`, `action-lighter` or `mixed`); a first version widens. A release that changes a rule adds an entry here and bumps the version in `crates/filter/src/packs/manifest.rs`.

## FK-1@1 (baseline)

First version, `widens` (against nothing). Matches a name that doesn't match the files, except disc images and scene RAR sets. Action: hide.

## FK-2@1 (baseline)

First version, `widens` (against nothing). Matches a tiny video claiming HD or 4K, except samples and trailers. Action: flag.

## FK-4@1 (baseline)

First version, `widens` (against nothing). Matches an empty or padding-only torrent. Action: drop.

## FK-5@1 (baseline)

First version, `widens` (against nothing). Matches an archive standing in for the media, except scene RAR sets and multi-part sets. Action: demote.
