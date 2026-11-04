# Lodestar Legal Notice and Network Risks

**Version 2 · 2026-11-04**

Lodestar shows this notice once, before first use, and asks you to confirm that you have read it. It explains what the software does on your network, what it finds, what is yours to decide, and what the makers do and don't promise. It is a notice, not a contract that limits what you may do with the software: your rights to run, study, change and share Lodestar come from its licence (section 1) and nothing here takes them away.

## 1. Who publishes Lodestar, and under what licence
Lodestar is published by its copyright holder, the project owner who maintains the repository at https://github.com/dvortsis/lodestar-v2, together with the contributors named in that repository's history.

The software is free software under the **GNU Affero General Public License, version 3 or any later version** (the "AGPL"). The AGPL lets you run, study, change and share it. You don't have to accept anything to run it; the confirmation Lodestar asks for is only that you have read this notice. The Lodestar name, logo, mascot and icons are **not** covered by the AGPL; `brand/LICENSE.md` says what you may do with them.

Sections 7 and 8 restate the AGPL's warranty disclaimer and limitation of liability (its sections 15 and 16) in plain words, as its section 7 permits. The rest of this notice is information and warnings.

## 2. What Lodestar does on your network
**By default, Lodestar collects torrent metadata and nothing else.** It takes part in the public BitTorrent DHT, learns which torrents exist, connects to peers, asks each for the torrent's description (the "info dictionary": names, file lists, sizes and hashes) and disconnects. It also asks trackers and the DHT how many peers a torrent has. **It does not request, store, share or seed the files inside a torrent.** It is not a download client.

Taking part in the DHT means other nodes and peers can see your public IP address, or your VPN's if you route Lodestar through one, as with any BitTorrent program. Organisations that monitor these networks can see it too. Lodestar does not store which peer had which torrent. The one exception is the optional **Peer activity** log kind, which writes to your local log file only while you turn it on.

**Optional sources are off until you turn them on,** and each shows its own notice first:
- **External** sources contact named public websites and send them search terms and info hashes.
- **Account** sources sign in with credentials you give them. Lodestar encrypts the credentials on your server, never shows or exports them, and keeps what those sources find on your server only.
- **Restricted** sources reach sites that may index copyrighted or otherwise restricted material, or announce your IP address to a torrent's other peers. They ask for your password every time they're turned on.
- **Overlay** sources join other peer-to-peer networks, with their own participants and content.

A future version may add a verification mode that fetches small fragments of a torrent's content **(planned, not built)**. If it ships, it will be off by default, behind its own notice and your password.

## 3. What Lodestar finds
Everything in your index was published by other people on public networks. Lodestar records what they published. It cannot check what any torrent really contains, and the networks carry material that may be infringing, unlawful, offensive, mislabelled or malicious.

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

It contacts the internet only for the DHT and peers, for the sources you turn on, for blocklist and rule-pack downloads you choose, and for update checks if you turn them on (those contact GitHub). A diagnostics bundle is created only when you ask for one, with torrent details removed, and goes only where you send it.

If your copy processes other people's personal data, for example IP addresses in logs or searches by people you let use it, data-protection law in your place may apply to you as the person running it.

## 6. If other people can reach your copy
If you make your Lodestar reachable by others, through its web interface, its API or a feed, **you are the operator of that service** and complaints about it come to you, not to the makers, who have no access to it. Publish a way to contact you, remove items when a valid complaint arrives, and read `docs/legal/operator-guidance.md` for the duties that may apply to a public index, including copyright notices and the reporting of child-exploitation material.

## 7. No warranty
Lodestar is free software. As the AGPL's section 15 says:

**THERE IS NO WARRANTY FOR THE PROGRAM, TO THE EXTENT PERMITTED BY APPLICABLE LAW. THE PROGRAM IS PROVIDED "AS IS" WITHOUT WARRANTY OF ANY KIND, EITHER EXPRESSED OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE. THE ENTIRE RISK AS TO THE QUALITY AND PERFORMANCE OF THE PROGRAM IS WITH YOU.** In particular, nobody promises that the index is accurate or complete, that the filters block any particular item, or that any result is lawful to access or use where you are. Nothing in the software or its documents is legal advice.

## 8. Limitation of liability
As the AGPL's section 16 says:

**IN NO EVENT, UNLESS REQUIRED BY APPLICABLE LAW OR AGREED TO IN WRITING, WILL ANY COPYRIGHT HOLDER OR CONTRIBUTOR BE LIABLE TO YOU FOR DAMAGES, INCLUDING ANY GENERAL, SPECIAL, INCIDENTAL OR CONSEQUENTIAL DAMAGES ARISING OUT OF THE USE OR INABILITY TO USE THE PROGRAM, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.** This includes what you or your users do with the software or with what it finds, the sources you connect it to, and any notice, claim or proceeding that follows.

This limitation applies only as far as the law allows. It does not limit liability for fraud, for wilful and wanton conduct, or for anything the law that applies to you says can't be limited, and it doesn't take away rights that consumer-protection law gives you. For New Jersey residents, no part of this notice limits liability for fraud, wilful or grossly negligent conduct, or violation of a statute, and sections 7 and 8 apply only as New Jersey law permits; every other section applies in full. For California residents, nothing here exempts anyone from liability for their own fraud, wilful injury or violation of law (Civil Code section 1668). Mandatory law of your country applies regardless of this notice.

## 9. Reporting and contact
- **Bugs, questions and complaints about the software or its documents:** open an issue at https://github.com/dvortsis/lodestar-v2.
- **Security vulnerabilities and sensitive reports,** including suspected child-exploitation material reaching an index: use the repository's private security-report form, not a public issue. **Never download suspected material to check it, and never attach it to a report.** In the United States, report child-exploitation material to the NCMEC CyberTipline (report.cybertip.org) or to law enforcement; elsewhere, to your national hotline.
- **Complaints about what a particular copy of Lodestar indexes or serves** go to the person who runs that copy (section 6).

## 10. Changes to this notice
The version and date are at the top. When a new version changes this notice in substance, your copy shows it again before you continue. A new version never changes your rights under the AGPL.

## 11. The law of this notice
This notice is given from Colorado, United States, and is read under Colorado law; disputes about it belong to the state or federal courts located in Colorado, unless the mandatory law of your place of residence gives you another forum or other protections. The AGPL is not affected by this section. If any part of this notice is unenforceable, the rest stands.
