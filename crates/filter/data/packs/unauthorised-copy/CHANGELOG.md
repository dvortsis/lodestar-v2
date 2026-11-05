# Unauthorised-copy pack changelog

One entry per rule version, newest first. Each names its direction against the version before (`widens`, `narrows`, `action-heavier`, `action-lighter` or `mixed`); a first version widens. A release that changes a rule adds an entry here and bumps the version in `crates/filter/src/packs/manifest.rs`.

The pack hides names that follow the conventions of unauthorised copies. It reads the release parser's fields, not a list of names, and it can't know who holds the rights (owner §2.39, RP-UC).

## UC-1@1

First version, `widens` (against nothing). Matches a film the parser reads as a camcorder, telesync, telecine or screener rip, when the name carries a strong source marker (`HDCAM`, `CAMRip`, `HD CAM`, `HDTS`, `HD TS`, `TELESYNC`, `PDVD`, `HDTC`, `TELECINE`, `DVDSCR`, `BDSCR`, `WEBSCR`, `screener`) or a weak one (`CAM`, `TS`, `TC`, `SCR`) in brackets or after the year. A weak marker on its own, as in `Nanny Cam 1080p x264-GRP`, doesn't match, nor does a name ending in the `.ts` extension of a transport-stream file the torrent holds. Action: hide.

## UC-2@1

First version, `widens` (against nothing). Matches cracked software: a software release with the parser's `cracked` flag and a crack word after its title (crack, cracked, keygen, keymaker, activator, k'ed, pre-activated). `patch` alone never matches, nor does `crack` or `cracked` before a screen or glass word (physical damage). The same test is MW-6 in the malware pack. Action: hide.

## UC-3@1

First version, `widens` (against nothing). Matches a scene-style film or TV rip: a film or TV classification, a disc or web source and a release group. Internet Archive items and the built-in allow list of public-domain and Creative Commons films are exempt. Action: hide.
