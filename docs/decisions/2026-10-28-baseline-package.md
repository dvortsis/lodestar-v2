# Clean-source native baseline packaging

The final pre-soak build must name its actual source revision. Locally commit
the reviewed integration slices, preserving the root and Settings checkouts.
`prepare_windows.py` refuses dirty source, builds with existing offline
dependencies and bounded Cargo storage, freezes the successful UI before Rust,
then validates the isolated packaging/shutdown fixture. Its final manifest binds
source commit/tree, executable, collector dependencies and all UI files. Failed
outputs are retained and never reused. This is not publication or soak launch.

Builds can expose narrow bundle margins even when an earlier build passed. Keep
all existing limits and preserve failed evidence. The final-build follow-up
shares Badge and Banner's identical status icon/ink/border maps in their common
UI layer. Their visual tokens and behavior are unchanged; component, route and
bundle gates still apply. No crawler scheduling or resource policy changes.

Docker Desktop may be gracefully stopped for the approved native baseline only
after verifying its local engine has no running containers. Keep stopped
containers and all data intact. A stopped-engine observation must be refreshed
at launch; this procedure never starts Docker or the public crawler.
