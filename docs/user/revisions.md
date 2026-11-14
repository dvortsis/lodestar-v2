# Revisions

| Version | Date | Change |
|---|---|---|
| 3.2 | 2026-11-13T22:58:00Z | Section 3.3: new part "Close gluetun's gap when the VPN drops": Lodestar's post-rules file for gluetun, how to add it to a gluetun you set up yourself and check it, the OpenVPN variant, and what to do if gluetun stops. The Lodestar-generated parts are newer than v0.5.1: the companion templates with the rules built in, and the Unraid template's Companion files path. Also a note on the ICMP answers your server may send when gluetun stops or restarts, and why the safe choice is to leave them, with no extra firewall rules on the server. That note is about gluetun itself, so it applies whatever Lodestar version you run. |
| 3.1 | 2026-11-12T12:00:00Z | New Chapter 8 on adding companions, all newer than v0.5.1: what a companion is (qBittorrent recommended), the four safety rules, Docker Compose service, Unraid template, managed install on Windows and Linux (and Windows qBittorrent's own installer), what Lodestar detects and sets up, adding a companion you already run, and a problem table. Appendix A: Chapter 8 answers. Glossary: companion. |
| 3.0 | 2026-11-12T08:00:00Z | New Chapter 7 on connecting other apps, all newer than v0.5.1: Torznab keys (one per app, shown once, regenerate and revoke), connecting Prowlarr with **API Path** left at `/api`, Sonarr's missing categories `5000` and `5045` when connected directly, what the apps' error messages mean, and the bitmagnet export with its end states and import command. Appendix A: Chapter 7 answers. Glossary: bitmagnet, indexer, Newznab category, Torznab, Torznab key. |
| 2.9 | 2026-11-11T20:00:00Z | Chapter 4: new section 4.8 on keeping Lodestar up to date on Windows, all newer than v0.5.1: the update setting and what Windows does with it, the install window, what Lodestar checks, the MSI and portable updates, probation and automatic rollback (what the screen shows, and what it doesn't) with the Event Viewer warning (event 4), what to do if a rollback fails, Restart now (planned in the page) with its one-day pause and the portable console, and problem rows. Check your understanding is now 4.9. Appendix B: update settings. Glossary: install window, probation, update rollback. |
| 2.8 | 2026-11-11T18:30:00Z | Chapter 4: the MSI options, service, uninstall and `REMOVE_DATA`; the portable ZIP and how it differs; the Linux service and `journalctl`; new sections 4.6 on the data folder warning and 4.7 on the Windows port warning, both marked newer than v0.5.1 where they are, and 4.8 with questions. Section 6.1: install-time rows for a stopped service, a taken port and a refused data folder. |
| 2.7 | 2026-11-11T11:35:00Z | Section 2.2: leave the v0.5.1 template's unused Hot path and Cold path fields empty. Section 2.4: `healthy` means only that the process is alive. Full speed starts after the 6-hour sampling wait: added to sections 2.5, 3.5 and 5.3 and to a row in section 6.1. Appendix B no longer lists the Hot path and Cold path fields. |
| 2.6 | 2026-11-11T11:26:11Z | Section 2.2: the unused Hot path and Cold path fields are gone from the Unraid templates, so the guide no longer describes them; Data holds everything and no other folder is needed. |
| 2.5 | 2026-11-07T23:59:00Z | Section 6.2: the new service code 536870915, the reason in Lodestar's own log, Repair for your own administrator account, and `icacls` for an administrator through a domain group or a work or school account. Section 6.1: a row on the Windows Public network profile, and the expected accounts on the `logs` folder. |
| 2.4 (v0.5.1) | 2026-11-07T03:35:37Z | Aligned the guide with the published tag and assets; corrected setup order, downloads, signatures, update guidance and feature availability. Earlier development entries below may describe later features. |
| 2.3 | 2026-11-06 | Section 3.7: `seal.key` unseals held-back names; `secrets.key` is not in the backup, so integration, proxy and notification secrets are entered again after a restore to a new server. Section 5.5: a webhook find becomes public once a public source reports it. |
| 2.2 | 2026-11-06 | Added section 4.4 on finding the setup token on Windows, section 6.3 on Windows service error 3 and three rows in section 6.1, and a glossary entry. Install on Linux is now 4.5, Get help is now 6.4. |
| 2.1 | 2026-11-05 | Added section 5.5 on connecting Sonarr and Radarr, a glossary entry, and corrected the list of sources in section 5.4. |
| 2.0 | 2026-11-05 | Section 2.5: nothing connects to the network until setup is done; the crawler starts on its own after setup. |
| 1.9 | 2026-11-05 | Added a section in 3.6 on pack versions, why a new version asks for your password, what "in doubt" means and overrides. |
| 1.8 | 2026-11-05 | Section 2.5: setup opens on the Legal Notice; reading it again in Settings → About; the one-time showing at sign-in. Section 1.2 no longer marks the setup step planned. |
| 1.7 | 2026-11-05 | Added section 6.2 on a Magnetico import refused as in use, and a row for it in section 6.1. |
| 1.6 | 2026-11-05 | Added section 5.4 on Settings → Sources, its rows in Appendix B, and two glossary entries. |
| 1.5 | 2026-11-04 | Added the lawful-use note and the Legal Notice link to section 1.2, and qualified the traffic takeaway. |
| 1.4 | 2026-11-02 | Added a note in section 3.3 and Appendix B that blocklist downloads ignore HTTPS_PROXY, and a glossary entry. |
| 1.3 | 2026-10-30 | Added section 3.8 on safe restart, the restart record folder and keeping it out of backups. |
| 1.2 | 2026-10-26 | Corrected optional Unraid logging guidance and the Advanced View illustration against the current templates. |
| 1.1 | 2026-10-24 | Added Chapter 5 screens, setup screen capture, Unraid form illustrations and horizontal logo lockup for v0.5.0. |
| 1.0 | 2026-10-22 | First version of the guide. |

<!-- nav:start -->
---

[← Glossary](glossary.md) · [Contents](README.md#contents) · [Index →](index.md)
<!-- nav:end -->
