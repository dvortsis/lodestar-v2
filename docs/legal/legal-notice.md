# Lodestar Legal Notice and Network Risks

**Version 3 · 2026-11-06**

*Applies to Lodestar 0.5.2 and the development builds that lead to it (the `main` branch). Where this notice says "this version", it means that software (a development build may report an earlier version number). The v0.5.1 release carries its own copy of this notice, version 2, written for what that build does.*

Lodestar shows this notice before first use, and again whenever its version is raised, and asks you to confirm that you have read it. It explains what the software does on your network, what it finds, what is yours to decide, and what the makers do and don't promise. It is a notice, not a contract that limits what you may do with the software: your rights to run, study, change and share Lodestar come from its licence (section 1) and nothing here takes them away.

## 1. Who publishes Lodestar, and under what licence
Lodestar is published by its copyright holder, **Benjamin Dvortsis**, who maintains the repository at https://github.com/dvortsis/lodestar-v2, together with the contributors named in that repository's history.

The software is free software under the **GNU Affero General Public License, version 3 or any later version** (the "AGPL"). The AGPL lets you run, study, change and share it. You don't have to accept anything to run it; the confirmation Lodestar asks for is only that you have read this notice. The Lodestar name, logo, mascot and icons are **not** covered by the AGPL; `brand/LICENSE.md` says what you may do with them.

Sections 7 and 8 summarise the AGPL's disclaimer of warranty and limitation of liability (its sections 15 and 16) in plain words. They add no terms to the licence. The licence text controls. The rest of this notice is information and warnings.

## 2. What Lodestar does on your network
**By default, Lodestar collects torrent metadata and nothing else.** It takes part in the public BitTorrent DHT, learns which torrents exist, connects to peers, asks each for the torrent's description (the "info dictionary": names, file lists, sizes and hashes) and disconnects. It also asks public trackers and the DHT how many peers a torrent has. **The crawler starts only after first-run setup is finished,** unless you pause it in Settings after setup, or start Lodestar with the environment variable `LODESTAR_DHT_ENABLED=false`, which keeps the DHT crawler off. **It does not request, store, share or seed the files inside a torrent.** It is not a download client. It can show you a torrent's magnet link and, if you set one up, hand it to a download client of your own; what that client then does happens outside Lodestar and is your decision.

Taking part in the DHT means other nodes and peers can see your public IP address, or your VPN's if you route Lodestar through one, as with any BitTorrent program. Organisations that monitor these networks can see it too. **Lodestar keeps no lasting record of which peer had which torrent.** Like every DHT node it holds a small in-memory table of peers that announced a torrent; an announcement expires 30 minutes after its most recent refresh, and the table is never written to disk. The one exception is the optional **Peer activity** log kind, which writes to your local log file only while you turn it on.

**This version has:** the DHT crawler; swarm counts from public trackers; a command-line tool that imports Magnetico and Bitmagnet databases; one External source, the **torrent cache**, which asks the public torrent-cache websites you list for a torrent's description; **Sonarr and Radarr webhooks**, which receive what your own Sonarr or Radarr grabbed (Lodestar sends them nothing, and everything they report is marked private); a hand-off to a download client you set up; and notification channels. Every optional source is **off until you turn it on.** The torrent cache can't be turned on until you accept the websites it contacts; the screen that asks isn't built yet, so today it can only be turned on through the API. The Sonarr and Radarr webhooks contact nothing and turn on with their switch in Settings → Sources. Being off by default is not the same as being absent: an optional source is code in your copy, and turning it on is your choice. The classes of source, built and planned:
- **External** sources contact named public websites and send them search terms or info hashes. Built: the torrent cache. Others are planned.
- **Account** sources sign in with credentials you give them. Lodestar encrypts the credentials on your server and never shows them again. None is built yet.
- **Restricted** sources announce your IP address to a torrent's other peers, or reach other high-risk services. They ask for your password every time they're turned on. None is built yet.
- **Overlay** sources join other peer-to-peer networks, with their own participants and content. None is built yet.

**What a private source finds is marked private for as long as only private sources have reported it.** A private item is never served through feeds or integrations, included in shareable exports, or used in public peer lookups. If a public source later reports the same torrent, the DHT or an import for example, Lodestar treats it as public from then on, because its description is public knowledge by then; what stays private is what only your private sources told you.

A future version may add a verification mode that fetches small fragments of a torrent's content **(planned, not built)**. If it ships, it will be off by default, behind its own notice and your password.

## 3. What Lodestar finds
What is in your index comes from what other people published on public networks, from private sources you connected, or from files you imported. Lodestar records what it was told. It cannot check what any torrent really contains, and these networks carry material that may be infringing, unlawful, offensive, mislabelled or malicious.

**Lodestar includes filters that reduce exposure to unwanted or potentially unlawful metadata.** A child-safety core runs on every source; its locked floor can't be switched off, and its other rules can be adjusted with your password because pattern rules sometimes catch lawful items. Rule packs filter malware, lures, fakes, spam, scams, and, with the unauthorised-copy pack, names that follow the patterns of unauthorised copies: camcorder rips, cracked software and scene-style rips of films and TV. **The packs are on by default when Lodestar starts with a new data folder; an install upgraded from an earlier version keeps its packs as they were.** Setup shows all of them for you to review; you can switch any pack off there, and any pack or rule off in Settings. The unauthorised-copy pack can't know who holds the rights to anything, so it also hides some lawful releases; an Allow rule brings one back. Blocklists you subscribe to are off until you choose them. **These filters can miss items and can wrongly classify lawful ones. Whether an item appears, is hidden or is labelled says nothing about whether it is lawful, genuine or safe.** Finding an item in your index gives you no right to download, share or use it. The networks also carry adult material; Lodestar hides it unless you turn on "Show adult content", and the software is not meant for minors.

## 4. What is yours to decide, and yours to answer for
You run this copy of Lodestar on your own hardware. The makers don't run it for you, can't see it and can't change it. That means:

- **What it connects to is your choice.** Turning on a source means you have read its notice and accept the risks it describes.
- **Who uses it is your choice.** Its password, API keys and network exposure are yours to protect, and what the people you let in do with it is yours to answer for.
- **What you do with what it finds is your sole responsibility.** Lodestar is made for lawful uses: research, network measurement, cataloguing and finding material you have the right to access, which includes uses that copyright law allows, such as fair use. Using it to infringe copyright, to break a law that applies to you or your server, to breach the terms of your internet, hosting or VPN provider or of a site it connects to, to track or profile other people, or to find or spread material that sexually exploits children is misuse that its makers don't support and that may be a crime. Seeing metadata about a copyrighted work is not itself infringement; what you do next may be.
- **The law where you and your server are is yours to know.** Laws on peer-to-peer networks, copyright, indexing, adult material and personal data differ by country and by state. Something lawful in one place may be unlawful in another. Taking part in these networks, and especially turning on sources that announce your address to other peers, can lead to notices from your internet provider, suspended accounts, civil claims or criminal investigation where you live. A VPN hides your address from peers; keeping it working is up to you.

## 5. Your data
Lodestar sends nothing to its makers: no telemetry, no crash reports, no usage data. Everything it stores is on your server, in your data folder, with three exceptions: the structured log files go to the folder you set under Settings → Logging (by default the `logs` folder inside the data folder); on Windows, if the service can't start or can't write its logs, it notes why, paths and account names only, in `%SystemRoot%\Temp\lodestar-service.log`; and the optional safe-restart switch keeps its restart record in a folder outside the data folder (user guide, section 3.8). The console output is kept wherever your system keeps it: Docker's container log, on Unraid too, or the journal under systemd. It stores:

- the index of torrent metadata and swarm counts;
- your settings, saved searches and alerts;
- API keys, kept as hashes, and the secrets for the integrations, proxies and notification channels you set up (and, when they ship, Account sources), encrypted with a key file kept outside the database;
- logs, which leave out torrent names unless you turn that on;
- a **query log** of searches, which is **off by default**, and which you can clear.

Credentials are encrypted at rest with a key file (`secrets.key`) stored separately from the database; Lodestar decrypts them when it uses them, so access to both the database and that file can expose them. **A backup made with `lodestar backup` contains the database, the configuration file, the admin password hash with the sign-in sessions and API keys, and the key that unseals quarantined names (`seal.key`).** It does not contain `secrets.key`: restoring it to a new installation brings the sessions and API keys back, and leaves the stored integration, proxy and notification secrets unreadable until you enter them again. Keep the backup on private storage, as the user guide says. A full copy of the data folder also contains the logs and every key file, `secrets.key` included. Shareable search exports exclude private findings.

Lodestar contacts the internet for the DHT and peers; for swarm counts from a built-in list of public trackers; for the sources you turn on; for blocklist downloads you choose; for notification channels you set up (ntfy, Discord, email, webhooks); and for update checks if you turn them on (those contact GitHub). The **Diagnostics** panel in Settings → About shows versions, platform and the names of changed settings, with no values, paths or torrent details; you can copy it, and nothing is sent unless you paste it somewhere.

If your copy processes other people's personal data, for example IP addresses in logs or searches by people you let use it, data-protection law in your place may apply to you as the person running it.

## 6. If other people can reach your copy
If you make your Lodestar reachable by others, through its web interface, its API or a feed, **you are the operator of that service** and complaints about it come to you, not to the makers, who have no access to it. Keep a working contact that you read, act on valid complaints, and read `docs/legal/operator-guidance.md` for the duties that may apply to a public index, including copyright notices and the reporting of child-exploitation material.

## 7. No warranty (a summary of AGPL section 15)
Lodestar is free software. There is no warranty for it, to the extent permitted by applicable law. Except when otherwise stated in writing, the copyright holders and contributors provide it **"as is"**, without warranty of any kind, express or implied, including the implied warranties of merchantability and fitness for a particular purpose. The entire risk as to its quality and performance is with you. In particular, nobody promises that the index is accurate or complete, that the filters block any particular item, or that any result is lawful to access or use where you are. Nothing in the software or its documents is legal advice. The licence text in `LICENSE` controls.

## 8. Limitation of liability (a summary of AGPL section 16)
Unless required by applicable law or agreed to in writing, no copyright holder or contributor will be liable to you for damages, including general, special, incidental or consequential damages, arising out of the use or inability to use the software, even if advised of the possibility of such damage. The licence text in `LICENSE` controls.

This limitation applies only as far as the law allows. It does not limit liability for fraud, for wilful and wanton conduct, or for anything the law that applies to you says can't be limited, and it doesn't take away rights that consumer-protection law gives you. For New Jersey residents, no part of this notice limits liability for fraud, wilful or grossly negligent conduct, or violation of a statute, and sections 7 and 8 apply only as New Jersey law permits; every other section applies in full. For California residents, nothing here exempts anyone from liability for their own fraud, wilful injury or violation of law (Civil Code section 1668). Mandatory law of your country applies regardless of this notice.

## 9. Reporting and contact
**Do not send reports of suspected illegal content to Lodestar's maintainers.** The maintainers develop the software; they do not operate independently hosted instances, investigate their indexed material, or handle removal requests for them.

- Send removal requests to the operator of the affected instance (section 6).
- Report suspected child sexual exploitation to the NCMEC CyberTipline (https://report.cybertip.org) in the United States, or to the appropriate authority elsewhere.
- Report other suspected crimes to the appropriate law-enforcement agency.

Do not send the maintainers suspected illegal files, screenshots, magnet links, hashes or identifying descriptions, and do not download suspected illegal material to investigate it. For software bugs or security vulnerabilities, use harmless synthetic examples that demonstrate the problem without identifying real suspected illegal material. Follow the reporting authority's instructions. These reporting directions do not override any independently applicable legal obligation, including a reporting or preservation duty that applies to you as the operator of a service (operator guidance, section 4).

**Security flaws in Lodestar's code:** use the project's private vulnerability reporting on GitHub (the repository's Security tab), not a public issue.

**Bugs, questions and everything else about the software or its documents:** open an issue at https://github.com/dvortsis/lodestar-v2.

## 10. Changes to this notice
The version and date are at the top. When a new version of this notice changes it in substance, your copy shows it again before you continue. A new version never changes your rights under the AGPL.

## 11. About this notice
This notice is given from Colorado, United States. It creates no agreement about where disputes are heard. If any part of it is unenforceable where you are, the rest stands, and the AGPL is not affected.
