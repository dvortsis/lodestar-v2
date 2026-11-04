# Scams pack changelog

One entry per rule version, newest first. Each names its direction against the version before (`widens`, `narrows`, `action-heavier`, `action-lighter` or `mixed`); a first version widens. A release that changes a rule adds an entry here and bumps the version in `crates/filter/src/packs/manifest.rs`.

## SC-1@1 (baseline)

First version, `widens` (against nothing). Matches "free premium", "generator" or "hack" for an online service (streaming, music, crypto, payments), except trainers, mods and offline cracks. Action: hide. Before RP-0 SC-1 also covered the online games SC-2 now covers; together the two match exactly what SC-1 matched then.

## SC-2@1 (baseline)

First version, `widens` (against nothing). Matches "generator" or "hack" for an online game's currency or accounts (Robux, V-Bucks, Roblox, Fortnite), except trainers, mods and offline cracks. Action: hide. Split out of SC-1 in RP-0 so it has its own switch and effect warning; an install that had SC-1 switched off or excepted starts with SC-2 the same.
