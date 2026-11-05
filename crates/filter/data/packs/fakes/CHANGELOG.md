# Fakes pack changelog

One entry per rule version, newest first. Each names its direction against the version before (`widens`, `narrows`, `action-heavier`, `action-lighter` or `mixed`); a first version widens. A release that changes a rule adds an entry here and bumps the version in `crates/filter/src/packs/manifest.rs`.

## FK-2@2

`narrows` against FK-2@1. Adds the short-episode guard §20.6 names ("short-episode pack"), which RP-1's precision corpus found missing. FK-2@1 still matches unless every video is an episode named `<series> SxxEyy`, there are at least three, all of one series with no episode twice, the torrent's name starts with that series, none is under 16 MiB and the largest is at most twice the smallest. A single tiny HD video is still matched. Action: flag.

## FK-1@1 (baseline)

First version, `widens` (against nothing). Matches a name that doesn't match the files, except disc images and scene RAR sets. Action: hide.

## FK-2@1 (baseline)

First version, `widens` (against nothing). Matches a tiny video claiming HD or 4K, except samples and trailers. Action: flag.

## FK-4@1 (baseline)

First version, `widens` (against nothing). Matches an empty or padding-only torrent. Action: drop.

## FK-5@1 (baseline)

First version, `widens` (against nothing). Matches an archive standing in for the media, except scene RAR sets and multi-part sets. Action: demote.
