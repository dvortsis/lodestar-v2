# X-sources-restructure decisions

The runner's choices where the owner's Sources decision (`docs/owner-decisions.md` §2.66 item 4; `docs/decisions/2026-11-13-runner-ui-review-decisions.md`, "Settings → Sources") leaves the detail open. None changes a consent dialog or what a switch does (§18.7, §10.10.7).

- **Groups start open; rows start closed.** On a first visit every source is listed, as before, and each row is one line. A closed group isn't remembered between visits. Why: nothing is hidden on arrival, while the one-line rows already make the page short.
- **Typing in the filter opens every group again.** Why: a match must never sit inside a closed group, where it would look like no match.
- **The filter matches the name and every kind a source has**, not only the group it's listed under. For example, "peer finders" finds the Mainline DHT, which is listed under Discovery. It ignores case.
- **"Needs attention" counts sources that failed, wait for a restart, have a purge that didn't finish, or are on and wait for a companion.** A source that's off, or that still needs setup before it can be turned on, doesn't count. Why: the summary should point at things to fix, not at every source the person chose to leave off.
- **The setup note and the companion chip stay on the row's line, not in the detail.** The setup note is the switch's description: it says why turning the source on opens a dialog instead. The chip is the one action a source waiting for a companion needs. Why: hiding either would leave the switch's behaviour, or the next step, out of sight until the row is opened. The rest of the detail (priority, addresses, last error, yield, Settings, Purge) opens with the row. It stays in the page while closed, so nothing it describes goes missing.
- **The legend calls the class with no consent "Open", and it has no badge on the rows.** §18.7 calls it "None". The rows show no badge for it, as before, so the legend says so.
- **The legend sits below the groups.** It's reference text, so it doesn't push the rows down on a phone.
