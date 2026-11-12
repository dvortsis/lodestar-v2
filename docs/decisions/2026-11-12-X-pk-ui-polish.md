# X-pk-ui-polish decisions

Nits from the PK-ui and PK-exc reviews. No owner number or safety decision changes.

- **Guide links carry anchors.** `data_folder` opens chapter 4 §4.6 (`#46-fix-a-data-folder-warning`) and `windows_ports` §4.7 (`#47-fix-the-windows-port-warning`), built the way the guide's other links are (GitHub heading slugs). This supersedes PK-ui's "page level only" and its `data_folder` → chapter 2 mapping. Chapter 2 §2.2 covers template settings, not this warning.
- **Insights has its own "(opens in a new tab)" key,** `insights_overview_exc_new_tab`, so it no longer depends on the About screen's string.
- **The storage step's warning sits in a polite `role="status"` region** that is rendered empty first and filled when `/system` answers.
- **Decisions files corrected:** PK-ui's repeated sentence removed, PK-exc's guide targets fixed to chapter 4, and both headings carry their PR number (#701, #687).
- **`ports.rs`:** a test pins the IPv4 `netsh` set and show command text. No behaviour change.
