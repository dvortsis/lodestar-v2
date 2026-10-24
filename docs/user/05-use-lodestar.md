# Chapter 5: Use Lodestar

Once you create the admin password, Lodestar opens to **Search** while the [DHT](glossary.md#dht) crawler collects [metadata](glossary.md#metadata) in the background.

> [!NOTE]
> **What you will learn**
> - Filter results with the facet rail or query tokens on **Search**.
> - Inspect a torrent's file tree and copy its magnet link in the detail drawer.
> - Browse newest discoveries on **Discover** and check pipeline health on **Insights**.

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

**Discover** lists the newest torrents indexed across all categories or within a single tab such as **Movies**, **TV** or **Music**.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/discover-dark.png">
  <img src="images/discover-light.png" alt="Lodestar Discover screen showing category tabs and newest indexed torrents"></picture>
*Figure 5.3: The Discover screen showing recently indexed torrents grouped by category.*

**Insights** shows the live health of the five-stage pipeline: **Discover**, **Fetch**, **Index**, **Storage** and **Refresh**, followed by searchable totals and attention notices.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/insights-dark.png">
  <img src="images/insights-light.png" alt="Lodestar Insights overview showing pipeline stage cards and status counters"></picture>
*Figure 5.4: The Insights overview showing stage status cards, searchable torrent counts and attention items.*

> [!TIP]
> **Key takeaways**
> - Combine text queries with facet checkboxes or `field:value` tokens on **Search**.
> - Select any row to inspect its file list and copy a magnet link.
> - Open **Insights** to check whether the DHT crawler and index are healthy.

<!-- nav:start -->
---

[← Chapter 4: Install on Windows and Linux](04-install-on-windows-and-linux.md) · [Contents](README.md#contents) · [Chapter 6: Troubleshooting →](06-troubleshooting.md)
<!-- nav:end -->
