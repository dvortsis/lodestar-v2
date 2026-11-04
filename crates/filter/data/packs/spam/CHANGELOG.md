# Spam pack changelog

One entry per rule version, newest first. Each names its direction against the version before (`widens`, `narrows`, `action-heavier`, `action-lighter` or `mixed`); a first version widens. A release that changes a rule adds an entry here and bumps the version in `crates/filter/src/packs/manifest.rs`.

## SP-1@1 (baseline)

First version, `widens` (against nothing). Matches a site advert before or after the title (`www.site.com - Title`, `[site.org]`). Action: demote.

## SP-2@1 (baseline)

First version, `widens` (against nothing). Matches a keyword-stuffed name over 300 characters, except music. Action: hide.
