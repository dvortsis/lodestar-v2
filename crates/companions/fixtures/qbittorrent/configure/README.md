# qBittorrent configuration fixtures (CMP-8b)

The starting state of the fake qBittorrent in `tests/configure.rs`, which
answers `/api/v2/app/preferences` and `/api/v2/torrents/categories` from
these files and changes its copy as `setPreferences`, `createCategory` and
`editCategory` arrive.

- `preferences.json`: the keys `appcontroller.cpp` writes at
  `release-5.0.3`, trimmed to about 80 of its 250, with linuxserver.io's
  defaults (`save_path` `/downloads/`, `web_ui_address` `*`, user `admin`).
  It keeps the five keys CMP-8b compares, a nested object (`scan_dirs`)
  and a list (`websocket_trackers`), so the bounded reader passes over
  both.
- The fake also answers as qBittorrent 5.2 does when a test asks
  (`release-5.2.0`'s `authcontroller.cpp` and `webapplication.cpp`): the
  `QBT_SID_<port>` cookie, `204` for a sign-in or a write, and `401` for a
  wrong password. The preference keys are the same from 4.6 to 5.2.
- `categories.json`: one category, in `categoriesAction`'s shape
  (`name`, `savePath`, `download_path`).

Reconstructed from upstream source, not captured from a pinned image: the
catalogue pins none yet (`docs/decisions/2026-11-08-CMP-8b.md`).
