# Lodestar Legal Notice and Network Risks

**Version 2 · 2026-11-04**

Lodestar shows this notice once, before first use, and asks you to confirm that you have read it. It explains what the software does on your network, what it finds, what is yours to decide, and what the makers do and don't promise. It is a notice, not a contract that limits what you may do with the software: your rights to run, study, change and share Lodestar come from its licence (section 1) and nothing here takes them away.

## 1. Who publishes Lodestar, and under what licence
Lodestar is published by its copyright holder, **Benjamin Dvortsis**, who maintains the repository at https://github.com/dvortsis/lodestar-v2, together with the contributors named in that repository's history.

The software is free software under the **GNU Affero General Public License, version 3 or any later version** (the "AGPL"). The AGPL lets you run, study, change and share it. You don't have to accept anything to run it; the confirmation Lodestar asks for is only that you have read this notice. The Lodestar name, logo, mascot and icons are **not** covered by the AGPL; `brand/LICENSE.md` says what you may do with them.

Sections 7 and 8 summarise the AGPL's disclaimer of warranty and limitation of liability (its sections 15 and 16) in plain words, as its section 7 permits. The licence text controls. The rest of this notice is information and warnings.

## 2. What Lodestar does on your network
**By default, Lodestar collects torrent metadata and nothing else.** It takes part in the public BitTorrent DHT, learns which torrents exist, connects to peers, asks each for the torrent's description (the "info dictionary": names, file lists, sizes and hashes) and disconnects. It also asks public trackers and the DHT how many peers a torrent has. **It does not request, store, share or seed the files inside a torrent.** It is not a download client. It can show you a torrent's magnet link and, if you set one up, hand it to a download client of your own; what that client then does happens outside Lodestar and is your decision.

Taking part in the DHT means other nodes and peers can see your public IP address, or your VPN's if you route Lodestar through one, as with any BitTorrent program. Organisations that monitor these networks can see it too. **Lodestar keeps no lasting record of which peer had which torrent.** Like every DHT node it holds a small in-memory table of peers that announced a torrent, which expires within half an hour and is never written to disk. The one exception is the optional **Peer activity** log kind, which writes to your local log file only while you turn it on.

**Optional sources are off until you turn them on,** and each shows its own notice first:
- **External** sources contact named public websites and send them search terms and info hashes.
- **Account** sources sign in with credentials you give them. Lodestar encrypts the credentials on your server and never shows them again. What they find through a private indexer or tracker is marked private and stays on your server: it is never served through feeds or integrations, included in shareable exports, or used in public peer lookups.
- **Restricted** sources announce your IP address to a torrent's other peers, or reach other high-risk services. They ask for your password every time they're turned on.
- **Overlay** sources join other peer-to-peer networks, with their own participants and content.

A future version may add a verification mode that fetches small fragments of a torrent's content **(planned, not built)**. If it ships, it will be off by default, behind its own notice and your password.

## 3. What Lodestar finds
What is in your index comes from what other people published on public networks, from private sources you connected, or from files you imported. Lodestar records what it was told. It cannot check what any torrent really contains, and these networks carry material that may be infringing, unlawful, offensive, mislabelled or malicious.

**Lodestar includes filters that reduce exposure to unwanted or potentially unlawful metadata.** A child-safety core runs on every source; its locked floor can't be switched off, and its other rules can be adjusted with your password because pattern rules sometimes catch lawful items. Optional rule packs and blocklists filter junk, malware and more. **These filters can miss items and can wrongly classify lawful ones. Whether an item appears, is hidden or is labelled says nothing about whether it is lawful, genuine or safe.** Finding an item in your index gives you no right to download, share or use it.

## 4. What is yours to decide, and yours to answer for
You run this copy of Lodestar on your own hardware. The makers don't run it for you, can't see it and can't change it. That means:

- **What it connects to is your choice.** Turning on a source means you have read its notice and accept the risks it describes.
- **Who uses it is your choice.** Its password, API keys and network exposure are yours to protect, and what the people you let in do with it is yours to answer for.
- **What you do with what it finds is your sole responsibility.** Lodestar is made for lawful uses: research, network measurement, cataloguing and finding material you have the right to access, which includes uses that copyright law allows, such as fair use. Using it to infringe copyright, to break a law that applies to you or your server, to breach the terms of your internet, hosting or VPN provider or of a site it connects to, to track or profile other people, or to find or spread material that sexually exploits children is misuse that its makers don't support and that may be a crime. Seeing metadata about a copyrighted work is not itself infringement; what you do next may be.
- **The law where you and your server are is yours to know.** Laws on peer-to-peer networks, copyright, indexing, adult material and personal data differ by country and by state. Something lawful in one place may be unlawful in another. Taking part in these networks, and especially turning on sources that announce your address to other peers, can lead to notices from your internet provider, suspended accounts, civil claims or criminal investigation where you live. A VPN hides your address from peers; keeping it working is up to you.

## 5. Your data
Lodestar sends nothing to its makers: no telemetry, no crash reports, no usage data. Everything it stores is in your data folder, on your server:

- the index of torrent metadata and swarm counts;
- your settings, saved searches and alerts;
- credentials for Account sources, encrypted with a key kept outside the database;
- logs, which leave out torrent names unless you turn that on;
- a **query log** of searches, which is **off by default**, and which you can clear.

Credentials are encrypted at rest with a key stored separately from the database; Lodestar decrypts them when it uses them, so access to both the database and the key can expose them. Database backups contain private findings and the encrypted credential values; a full copy of the data folder can also contain the key files and logs. Protect your server and your backups accordingly. Shareable search exports exclude private findings.

Lodestar contacts the internet for the DHT and peers; for swarm counts from a built-in list of public trackers; for the sources you turn on; for blocklist and rule-pack downloads you choose; for notification channels you set up (ntfy, Discord, email, webhooks); and for update checks if you turn them on (those contact GitHub). The **Diagnostics** panel in Settings → About shows versions, platform and the names of changed settings, with no values, paths or torrent details; you can copy it, and nothing is sent unless you paste it somewhere.

If your copy processes other people's personal data, for example IP addresses in logs or searches by people you let use it, data-protection law in your place may apply to you as the person running it.

## 6. If other people can reach your copy
If you make your Lodestar reachable by others, through its web interface, its API or a feed, **you are the operator of that service** and complaints about it come to you, not to the makers, who have no access to it. Keep a working contact that you read, act on valid complaints, and read `docs/legal/operator-guidance.md` for the duties that may apply to a public index, including copyright notices and the reporting of child-exploitation material.

## 7. No warranty (a summary of AGPL section 15)
Lodestar is free software. There is no warranty for it, to the extent permitted by applicable law. Except when otherwise stated in writing, the copyright holders and contributors provide it **"as is"**, without warranty of any kind, express or implied, including the implied warranties of merchantability and fitness for a particular purpose. The entire risk as to its quality and performance is with you. In particular, nobody promises that the index is accurate or complete, that the filters block any particular item, or that any result is lawful to access or use where you are. Nothing in the software or its documents is legal advice. The licence text in `LICENSE` controls.

## 8. Limitation of liability (a summary of AGPL section 16)
Unless required by applicable law or agreed to in writing, no copyright holder or contributor will be liable to you for damages, including general, special, incidental or consequential damages, arising out of the use or inability to use the software, even if advised of the possibility of such damage. This includes what you or your users do with the software or with what it finds, the sources you connect it to, and any notice, claim or proceeding that follows. The licence text in `LICENSE` controls.

This limitation applies only as far as the law allows. It does not limit liability for fraud, for wilful and wanton conduct, or for anything the law that applies to you says can't be limited, and it doesn't take away rights that consumer-protection law gives you. For New Jersey residents, no part of this notice limits liability for fraud, wilful or grossly negligent conduct, or violation of a statute, and sections 7 and 8 apply only as New Jersey law permits; every other section applies in full. For California residents, nothing here exempts anyone from liability for their own fraud, wilful injury or violation of law (Civil Code section 1668). Mandatory law of your country applies regardless of this notice.

## 9. Reporting and contact
**Reports about indexed material.** Lodestar's maintainers distribute the software and do not operate independently hosted instances. They cannot remove entries from those instances or determine who originally published the underlying material.

- Report suspected child sexual exploitation directly to the NCMEC CyberTipline (https://report.cybertip.org) in the United States, or to the appropriate reporting authority in your country.
- Report other suspected crimes to the appropriate law-enforcement agency.
- Requests to remove entries from a particular instance go to that instance's operator (section 6).

Do not download suspected illegal material to investigate it, send it to Lodestar's maintainers, or attach it to public issues. Follow the reporting authority's instructions. This section directs reports; it cannot waive a reporting or preservation duty that independently applies to you as the operator of a service (operator guidance, section 4).

**Security flaws in Lodestar's code:** use the project's private vulnerability reporting on GitHub (the repository's Security tab), not a public issue.

**Bugs, questions and everything else about the software or its documents:** open an issue at https://github.com/dvortsis/lodestar-v2.

## 10. Changes to this notice
The version and date are at the top. When a new version changes this notice in substance, your copy shows it again before you continue. A new version never changes your rights under the AGPL.

## 11. About this notice
This notice is given from Colorado, United States. It creates no agreement about where disputes are heard. If any part of it is unenforceable where you are, the rest stands, and the AGPL is not affected.
