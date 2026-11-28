# X-detail-attributes-ui decisions

- **The Release list sits in the Parsed section, not under Technical details** (runner, step 0, §2.26). §10.5 item 3 is where parsed metadata lives, and an expert-only disclosure would hide it from the people it helps. This refines the brief's default.
- **A "Release" `<dl>` under Parsed's rows**, from `TorrentDetail.attributes`. Null and `{}` show nothing. A key without a label is dropped, never shown raw.
- **Overlap with the title identity rows.** Parsed already shows year, season, episode and absolute from the siblings answer (the title's spans across versions). The list leaves a key out when its row is shown, and shows this torrent's own value (Year, Season, Episode, Absolute episode) when it isn't. `title` shows only when it differs from the resolved title.
- **Formatters:** `lang` and `subs` through `Intl.DisplayNames` in the UI locale (`multi` is "Multiple"), `bits` as "10-bit", `pack: yes` as "Season or collection pack", `service` through a ten-name table (about 130 B gzip, under the 200 B bar). Every other value shows as sent. No `source` table.
- **zh strings are machine-drafted** and need a human pass.
- **Glossary:** `release_group`, `container_format` and `release_tags`.
- **Size** (measured, rounded up to 64 B; main passes all three at its own limits): Detail sections 47.52 KB → 49,024 B (measured 48,997 B, +1.48 kB), Detail drawer 28.27 KB → 28,288 B (28,283 B), /t/[hash] 15.71 KB → 15,744 B (15,718 B).
