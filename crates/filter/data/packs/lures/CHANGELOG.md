# Lures pack changelog

One entry per rule version, newest first. Each names its direction against the version before (`widens`, `narrows`, `action-heavier`, `action-lighter` or `mixed`); a first version widens. A release that changes a rule adds an entry here and bumps the version in `crates/filter/src/packs/manifest.rs`.

## LU-1@1 (baseline)

First version, `widens` (against nothing). Matches a password lure file with an archive standing in for the media. Action: hide.

## LU-2@1 (baseline)

First version, `widens` (against nothing). Matches only "how to open / visit / download from" notes and an archive, without real media. Action: demote.

## LU-3@1 (baseline)

First version, `widens` (against nothing). Matches a web shortcut (`.url`) in media. Action: demote.

## LU-4@1 (baseline)

First version, `widens` (against nothing). Matches "unlock", "survey" or "human verification" notes. Action: hide.
