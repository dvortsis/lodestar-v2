<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../../brand/source/logos/horizontal_dark_transparent.png">
    <source media="(prefers-color-scheme: light)" srcset="../../brand/source/logos/horizontal_light_transparent.png">
    <img src="../../brand/source/logos/horizontal_light_transparent.png" alt="Lodestar logo" width="380">
  </picture>
</p>

# Lodestar setup guide

Install Lodestar, set it up safely and keep it running.

## Who this guide is for

You can install an app from the Unraid app store or start a Docker container. This guide adds what the install form doesn't tell you: what each field means, how Docker treats your files and how to keep Lodestar private.

## How to use this guide

Read chapters 1 to 3 in order the first time. Come back to a chapter by task. Anything marked **(planned)** isn't built yet.

### Where to find each task

| If you want to | Go to |
|---|---|
| Learn what Lodestar does and never does | [Chapter 1](01-before-you-start.md) |
| Install Lodestar on Unraid | [Chapter 2](02-install-lodestar-on-unraid-and-docker.md) |
| Install Lodestar with Docker Compose | [Chapter 2](02-install-lodestar-on-unraid-and-docker.md#24-install-with-docker-compose) |
| Create the admin password | [Chapter 2](02-install-lodestar-on-unraid-and-docker.md#25-create-the-admin-password) |
| Reach Lodestar from outside my home | [Chapter 3](03-keep-lodestar-safe.md#31-keep-the-web-ui-off-the-internet) |
| Run Lodestar through a VPN | [Chapter 3](03-keep-lodestar-safe.md#33-decide-about-the-dht-port-and-a-vpn) |
| Back up and restore Lodestar | [Chapter 3](03-keep-lodestar-safe.md#37-back-up-and-restore) |
| Install on Windows or Linux | [Chapter 4](04-install-on-windows-and-linux.md) |
| Search, browse and check crawler health | [Chapter 5](05-use-lodestar.md) |
| Fix a problem | [Chapter 6](06-troubleshooting.md) |

**Reading paths.** First-time installers read Chapters 1 to 3 in order. Returning readers use the table above to jump to a task, backup step or fix.

## Contents

<!-- contents:start -->
- [Chapter 1: Before you start](01-before-you-start.md)
  - [1.1 Learn what Lodestar does](01-before-you-start.md#11-learn-what-lodestar-does)
  - [1.2 Know what Lodestar never does](01-before-you-start.md#12-know-what-lodestar-never-does)
  - [1.3 Check what you need](01-before-you-start.md#13-check-what-you-need)
  - [1.4 Choose how to install](01-before-you-start.md#14-choose-how-to-install)
  - [1.5 Check your understanding](01-before-you-start.md#15-check-your-understanding)
- [Chapter 2: Install on Unraid and Docker](02-install-lodestar-on-unraid-and-docker.md)
  - [2.1 Install with the Unraid template](02-install-lodestar-on-unraid-and-docker.md#21-install-with-the-unraid-template)
  - [2.2 Settings in the Unraid template](02-install-lodestar-on-unraid-and-docker.md#22-settings-in-the-unraid-template)
  - [2.3 Check the user the container runs as](02-install-lodestar-on-unraid-and-docker.md#23-check-the-user-the-container-runs-as)
  - [2.4 Install with Docker Compose](02-install-lodestar-on-unraid-and-docker.md#24-install-with-docker-compose)
  - [2.5 Create the admin password](02-install-lodestar-on-unraid-and-docker.md#25-create-the-admin-password)
  - [2.6 Check your understanding](02-install-lodestar-on-unraid-and-docker.md#26-check-your-understanding)
- [Chapter 3: Keep Lodestar safe](03-keep-lodestar-safe.md)
  - [3.1 Keep the web UI off the internet](03-keep-lodestar-safe.md#31-keep-the-web-ui-off-the-internet)
  - [3.2 Protect the setup token and the password](03-keep-lodestar-safe.md#32-protect-the-setup-token-and-the-password)
  - [3.3 Decide about the DHT port and a VPN](03-keep-lodestar-safe.md#33-decide-about-the-dht-port-and-a-vpn)
  - [3.4 Keep logs private](03-keep-lodestar-safe.md#34-keep-logs-private)
  - [3.5 Choose how updates work](03-keep-lodestar-safe.md#35-choose-how-updates-work)
  - [3.6 Know what protects the content filters and API keys](03-keep-lodestar-safe.md#36-know-what-protects-the-content-filters-and-api-keys)
  - [3.7 Back up and restore](03-keep-lodestar-safe.md#37-back-up-and-restore)
  - [3.8 Check your understanding](03-keep-lodestar-safe.md#38-check-your-understanding)
- [Chapter 4: Install on Windows and Linux](04-install-on-windows-and-linux.md)
  - [4.1 Understand the SmartScreen warning](04-install-on-windows-and-linux.md#41-understand-the-smartscreen-warning)
  - [4.2 Check the download before you run it](04-install-on-windows-and-linux.md#42-check-the-download-before-you-run-it)
  - [4.3 Run it or stop](04-install-on-windows-and-linux.md#43-run-it-or-stop)
  - [4.4 Install on Linux](04-install-on-windows-and-linux.md#44-install-on-linux)
- [Chapter 5: Use Lodestar](05-use-lodestar.md)
  - [5.1 Search and filter](05-use-lodestar.md#51-search-and-filter)
  - [5.2 Inspect a torrent](05-use-lodestar.md#52-inspect-a-torrent)
  - [5.3 Browse Discover and Insights](05-use-lodestar.md#53-browse-discover-and-insights)
- [Chapter 6: Troubleshooting](06-troubleshooting.md)
  - [6.1 Common problems](06-troubleshooting.md#61-common-problems)
  - [6.2 Get help](06-troubleshooting.md#62-get-help)
- [Appendix A: Answers to check your understanding](appendix-a-answers.md)
- [Appendix B: Settings reference](appendix-b-settings-reference.md)
- [Glossary](glossary.md)
- [Revisions](revisions.md)
- [Index](index.md)
<!-- contents:end -->

## What you need

- A server that runs Unraid or Docker.
- Fast local storage (SSD or NVMe) for the data folder.
- Port `8090` free on the server for the web page.
- A password manager for the admin password.

## About this guide

This guide covers Lodestar v0.5.1 (draft). Last updated 2026-10-30. Facts come from `packaging/unraid/lodestar.xml`, `packaging/unraid/lodestar-gluetun.xml`, `packaging/docker/README.md`, `docker-compose.yml`, `docs/operations.md` and `docs/architecture.md` §12. They are listed in [`docs/setup.facts.md`](../setup.facts.md). Form illustrations in Chapter 2 are generated from `packaging/unraid/lodestar.xml`; see [`images/SHOTLIST.md`](images/SHOTLIST.md) for live capture instructions.

<!-- nav:start -->
---

[Chapter 1: Before you start →](01-before-you-start.md)
<!-- nav:end -->
