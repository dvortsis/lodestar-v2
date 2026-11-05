# Chapter 5: Use Lodestar

Once you create the admin password, Lodestar opens to **Search** while the [DHT](glossary.md#dht) crawler collects [metadata](glossary.md#metadata) in the background.

> [!NOTE]
> **What you will learn**
> - Filter results with the facet rail or query tokens on **Search**.
> - Inspect a torrent's file tree and copy its magnet link in the detail drawer.
> - Browse newest discoveries on **Discover** and check pipeline health on **Insights**.
> - Choose which sources Lodestar uses to find torrents in **Settings → Sources**.

## 5.1 Search and filter
<!-- index: Search; facets; query syntax; composition bar -->

The **Search** screen pairs a query bar with a left-hand facet rail for type, discovery window, size, file extension and language. Each row shows a parsed title, content-type badge, file-composition bar and total size.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/search-dark.png">
  <img src="images/search-light.png" alt="Lodestar Search screen showing facet rail histograms and result composition bars"></picture>
*Figure 5.1: The Search screen filtered by resolution, with facet histograms on the left and file composition bars on each row.*

Select any value in the facet rail to include it, or select the minus button beside a count to exclude it. Select **Syntax** beside the search box to view operators such as `res:1080p`, `type:movie` or `ext:mkv`.

## 5.2 Inspect a torrent
<!-- index: detail drawer; magnet link; file tree -->

Select a result row to open its detail drawer on the right without leaving your search results. The drawer shows swarm numbers, piece size, flags and the folder tree inside the torrent.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/detail-dark.png">
  <img src="images/detail-light.png" alt="Lodestar torrent detail drawer open beside the search results list"></picture>
*Figure 5.2: The torrent detail drawer showing summary fields, magnet actions and the contents tree.*

Select **Copy magnet** to copy the magnet URI to your clipboard, or **Open full page** to open the torrent at its own URL.

## 5.3 Browse Discover and Insights
<!-- index: Discover; Insights; crawler health -->

**Discover** lists the newest torrents indexed across all categories or within a single tab such as **Movie**, **TV** or **Music**.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/discover-dark.png">
  <img src="images/discover-light.png" alt="Lodestar Discover screen showing category tabs and newest indexed torrents"></picture>
*Figure 5.3: The Discover screen showing recently indexed torrents grouped by category.*

**Insights** shows the live health of the five-stage pipeline: **Discover**, **Fetch**, **Understand**, **Index** and **Serve**, followed by searchable totals and attention notices.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/insights-dark.png">
  <img src="images/insights-light.png" alt="Lodestar Insights overview showing pipeline stage cards and status counters"></picture>
*Figure 5.4: The Insights overview showing stage status cards, searchable torrent counts and attention items.*

## 5.4 Choose where Lodestar finds torrents (Settings → Sources)
<!-- index: sources; Settings → Sources; source status; source priority; yield bar -->

A **source** is a place Lodestar learns about torrents from. The [DHT](glossary.md#dht) is the one it uses out of the box. Other sources, such as a torrent site or an indexer app you already run, can add more. Lodestar still collects [metadata](glossary.md#metadata) only and never downloads torrent content.

To see them, select **Settings**, then **Sources** under **Crawling**.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/sources-dark.png">
  <img src="images/sources-light.png" alt="Lodestar Settings Sources screen listing Mainline DHT, Nyaa and Prowlarr in groups, each with a status, an On switch and a Priority menu"></picture>
*Figure 5.5: The Sources screen. The outlined row is a source that needs setup. Source names here are examples.*

**Groups.** Sources sit under the kind of work they do: **Discovery**, **Importers**, **Peer finders**, **Enrichment**, **Networks** and **Outputs**. A source with two kinds appears once, under the first. Each row also shows a risk label (**Low risk**, **Medium risk** or **High risk**). Some rows show a class label: **External** means it contacts other sites, and **Account** means it needs your account details.

**Status.** Each row has a status with an icon and words:

| Status | What it means |
|---|---|
| **Running** | On, and working now. |
| **On, not running now** | On, and waiting for its next turn. This is normal. |
| **Stopped with an error** | On, but it stopped. The row says why. Lodestar keeps running. |
| **Needs setup** | Off, and it can't be turned on yet. See below. |
| **Paused** | Off, kept as you left it. |
| **Off** | Off. |
| **Purged** | Its data was removed. |
| **Purge unfinished** | A removal of its data didn't finish. |

**Turn a source on or off.** Use the **On** switch on its row. Lodestar saves the change at once and says so on the screen. If the row shows **Applies at next start**, the source starts or stops the next time you restart Lodestar.

**Needs setup.** Some sources must be set up before they can be turned on. Setup means accepting the sites they contact, or adding account details. Their switch stays off and the row says what is missing. Setting these up is not on this screen yet. It is coming in a later version, so leave those sources off for now.

**Priority.** The **Priority** menu (**Low**, **Normal**, **High** or **Maximum**) sets how much of Lodestar's fetch work a source gets compared with the others. Leave it on **Normal** unless one source is crowding out the rest.

**The yield bar.** The small bar under each row shows how much of what the source found Lodestar stored, over the last 24 hours. The words beside it give the counts: **unique finds**, which are torrents this source reported first, and **of them stored**. A short bar with a high find count is normal, because Lodestar skips what it already has.

> [!IMPORTANT]
> **Recommended:** leave sources that contact other sites (**External**) or need your account (**Account**) turned off unless you need them. Each one sends requests from your connection to a place outside the DHT.

> [!NOTE]
> Changes on this screen apply at once. Unlike other settings, there is no **Undo**. To reverse a change, set the switch or menu back.

> [!TIP]
> **Key takeaways**
> - Combine text queries with facet checkboxes or `field:value` tokens on **Search**.
> - Select any row to inspect its file list and copy a magnet link.
> - Open **Insights** to check whether the DHT crawler and index are healthy.
> - Leave **External** and **Account** sources off unless you need them. Source changes apply at once, with no **Undo**.

<!-- nav:start -->
---

[← Chapter 4: Install on Windows and Linux](04-install-on-windows-and-linux.md) · [Contents](README.md#contents) · [Chapter 6: Troubleshooting →](06-troubleshooting.md)
<!-- nav:end -->
