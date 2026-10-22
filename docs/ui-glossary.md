# UI glossary

The words the UI uses for Lodestar's concepts (architecture §10.14). Every
screen, message, tooltip, help page and digest uses these labels exactly, and
never a synonym ("swarm size" for seeders, "hash" for torrent, "spam" for
junk). Metric definitions and formatting live in `docs/metrics.md`; this file
fixes the words.

- **en** is the settled label. Sentence case everywhere ("Trackers reporting",
  not "Trackers Reporting").
- **zh**: no Chinese label is settled yet. The zh column holds a **proposal**
  for the translator (W4); every entry is **TBD** until the owner or a
  reviewer confirms it, and then this file is updated and the marker removed.
- Message keys (Paraglide) use the term's key column, so a label changes in
  one place.

## 1. Core terms (§10.14)

| Key | en label | Definition | zh (proposed, TBD) |
|---|---|---|---|
| `torrent` | Torrent | One info dict, identified by its info hash (v1, or truncated v2; a hybrid is one torrent). Never "hash", "item" or "result" in labels. | 种子 (TBD) |
| `file` | File | One entry in a torrent's file list, with a path and a size. Padding files are files, hidden by default. | 文件 (TBD) |
| `resolved_title` | Resolved title | The name Lodestar shows: parsed from the torrent name or, when that's uninformative, resolved from its files, folder, structure or a plugin, with the method shown ("resolved via files"). | 解析标题 (TBD) |
| `raw_name` | Raw name | The torrent's own `name` field, decoded, shown in mono when it differs from the resolved title. | 原始名称 (TBD) |
| `discovered` | Discovered | When Lodestar first saw the torrent's key, not when it was created. Torrents first seen during the warm-up window carry "First seen during backfill". | 发现时间 (TBD) |
| `last_seen` | Last seen | The latest time the torrent's key was seen active in the DHT. | 最后出现 (TBD) |
| `seeders` | Seeders | Peers with the complete torrent: the highest count among sources within the freshness window. "—" means never scraped; 0 means scraped and nobody seeding. | 做种数 (TBD) |
| `trackers_reporting` | Trackers reporting | Trackers whose last scrape reported any seeders, leechers or completions for this torrent, out of the trackers that answered the last answered scrape round ("4 / 20"; "Reported by 4 of 20 trackers checked"). | 报告的 Tracker (TBD) |
| `junk` | Junk | A torrent whose junk score (0–100, with reasons) is over the threshold: lure files, executables in media, mismatched names and similar. Hidden by default ("N hidden · Show"). | 垃圾 (TBD) |
| `version_group` | Version group | Torrents of the same release in different versions (resolution, codec, edition, group). "Group versions" collapses them. | 版本组 (TBD) |
| `clone` | Clone | A torrent with an identical payload (same files and contents) to another, under a different info hash. | 克隆 (TBD) |

## 2. Related labels

Terms the core terms depend on, and the truthful-state strings from §10.14.

| Key | en label | Definition | zh (proposed, TBD) |
|---|---|---|---|
| `leechers` | Leechers | Peers still downloading. Same null/zero rules as seeders. | 下载数 (TBD) |
| `completed` | Completed | Downloads trackers report as finished. | 完成数 (TBD) |
| `trackers_checked` | Trackers checked | Trackers that **answered** in the last answered scrape round (the "m" in "n of m"). Trackers that didn't answer aren't counted, and a round where none answered changes nothing. | 已检查的 Tracker (TBD) |
| `dht_estimate` | DHT estimate | Seeds and peers estimated from the DHT (BEP 33). Always shown as an estimate ("≈"). | DHT 估计 (TBD) |
| `never_scraped` | Never scraped | No swarm data yet; shown as "—" in rows. | 从未抓取 (TBD) |
| `scrape_age` | Seen {relative} | How old the swarm data is. `{relative}` is the full `formatRelative` output, which already carries "ago" ("Seen 3 hours ago", "Seen yesterday"). | 检查于{relative} (TBD; e.g. 检查于3小时前) |
| `backfill` | First seen during backfill | The discovered date only means "when Lodestar first saw it" (warm-up window). | 回填期间首次发现 (TBD) |
| `estimate` | ≈ | Prefix for an estimated value ("≈ 2.4M"). The accessible name is "about". | ≈ / 约 (TBD) |
| `sampled_note` | Counts are estimated from a sample | Note on a sampled facet panel. | 计数基于抽样估计 (TBD) |
| `incomplete` | Incomplete | A discovery cohort that hasn't finished (not yet final). | 未完成 (TBD) |
| `swarm_suspect` | Suspect seeders | One tracker reports a swarm nobody else sees. | 可疑做种数 (TBD) |
| `composition` | Composition | The share of a torrent's bytes (not counting padding files) in each file category group ("82% video"). | 内容构成 (TBD) |
| `category_group` | File type | A colour group of file categories: Video, Audio, Images, Documents, Archives, Software, Other. | 文件类型 (TBD) |
| `content_type` | Type | What the torrent is: Movie, TV, Music, Audiobook, Ebook, Comic, Software, Game, Images, Video, Other, Adult. | 类型 (TBD) |
| `adult_kind` | Adult type | The kind of adult item: Scene, Movie, Photos, Comics, Game, VR, Audio (§20.9). Shown only when "Show adult content" is on. | 成人类型 (TBD) |
| `hidden_by_filters` | Hidden by filters | Hidden by a content-filter rule, with a "why" per item (§20). Distinct from junk. | 已被过滤器隐藏 (TBD) |
| `searchable` | Searchable | Indexed and returned by search. | 可搜索 (TBD) |
| `harvested` | Harvested | A key Lodestar collected from a discovery source (e.g. the DHT), before any metadata. | 已收集 (TBD) |
| `yield` | Yield | The share of harvested keys that became searchable (completed cohorts only). | 转化率 (TBD) |
| `in_library` | In library | Already in your download client or library. | 已在库中 (TBD) |

## 3. File categories and groups

Category labels (facets, file tree) and the group each colours as
(`docs/design-tokens.md` §3).

| Category key | en label | Group | zh (proposed, TBD) |
|---|---|---|---|
| `video` | Video | Video | 视频 (TBD) |
| `audio` | Audio | Audio | 音频 (TBD) |
| `image` | Image | Images | 图片 (TBD) |
| `document` | Document | Documents | 文档 (TBD) |
| `ebook` | Ebook | Documents | 电子书 (TBD) |
| `subtitle` | Subtitle | Documents | 字幕 (TBD) |
| `archive` | Archive | Archives | 压缩包 (TBD) |
| `disc` | Disc image | Archives | 光盘镜像 (TBD) |
| `software` | Software | Software | 软件 (TBD) |
| `other` | Other | Other | 其他 (TBD) |

## 4. Words not to use

| Don't write | Write |
|---|---|
| hash, item, entry (for a torrent) | torrent |
| spam, fake (for junk) | junk |
| peers (for seeders) | seeders / leechers |
| trackers (alone, for the count) | trackers reporting |
| added, created, uploaded (for discovered) | discovered |
| duplicate (for an identical payload) | clone |
| title (when it's the raw name) | raw name |
| 0 (when never scraped) | — / Never scraped |

## 5. Settings labels (§10.10)

| Key | en label | Definition | zh (proposed, TBD) |
|---|---|---|---|
| `setting_default` | Default | The value Lodestar ships with; nothing overrides it. | 默认 (TBD) |
| `setting_file` | From config.toml | Set in `config.toml`; a value saved in the app overrides it. | 来自 config.toml (TBD) |
| `setting_saved` | Saved in app | Saved from Settings. Reset to default removes it. | 已在应用中保存 (TBD) |
| `setting_env` | Set by {variable} | Set by a `LODESTAR_*` environment variable. It wins over everything, so the field is locked. | 由 {variable} 设置 (TBD) |
| `setting_restart` | Applies after restart | Saved, but the running server uses the old value until it restarts. | 重启后生效 (TBD) |
| `setting_changed` | Changed from default | Any value other than the default, whatever its source. | 已更改 (TBD) |
| `setting_automatic` | Automatic | The value follows another setting; the effective value is shown next to it ("Automatic · 400 at Balanced"). | 自动 (TBD) |
| `log_event` | Log event | One kind of thing that can be written to the log ("Each failed metadata fetch"), which you can turn on or off. | 日志事件 (TBD) |
| `log_data_kind` | Data in log lines | One kind of information a log line may contain (info hashes, torrent names, file names and paths…), switched per output. | 日志内容 (TBD) |
| `log_private` | Private | The default data preset: no torrent or peer identifiers in any line. | 私密 (TBD) |
| `live_log` | Live log | The page that follows new log lines as they're written (`/logs`). | 实时日志 (TBD) |
| `full_log` | Log files | The log files on disk, listed in Settings → Logging, for download. | 日志文件 (TBD) |
| `update_fix` | Fix | A release with bug and security fixes only: no data migration, no reindex, no config change. | 修复 (TBD) |
| `update_feature` | Feature | A release that may migrate data (after a snapshot) or rebuild the search index in the background. | 功能 (TBD) |
| `update_major` | Major | A release that may change config or behaviour; read its notes first. | 重大 (TBD) |
| `update_rolled_back` | Rolled back | An update that failed its health check and was undone automatically. | 已回滚 (TBD) |
| `needs_attention` | Needs attention | The Overview's list of problems, each with one action that fixes it. | 需要注意 (TBD) |

Don't write "env", "override", "custom" or "dirty" in these labels.
