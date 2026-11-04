# Running a Lodestar that other people can reach

This page is for the person who makes a Lodestar instance reachable by others: through its web interface, its API, a Torznab feed or an integration. Publishing the software and running a public index are different things. The makers do the first; if you do the second, the duties below may fall on you. **This is guidance, not legal advice.** What applies to you depends on where you and your users are and on what your service does; ask a lawyer before you open an index to the public.

## 1. Decide whether you need to be public at all
Lodestar is designed for a private instance behind a password, reached over a LAN or a VPN (user guide, chapter 3). Most of this page doesn't apply to that. A public, unauthenticated index changes your position: you become the operator of an online service that lists what strangers published.

## 2. Publish a contact
Give people a way to reach you that you read: an email address or a form, on the page itself or in the feed's description. Complaints that can't reach you still count against you. Public GitHub issues on the Lodestar project don't reach you and aren't a substitute.

## 3. Handle complaints about indexed items
- **Removing an item.** Use **Block this…** on the item, in the search results or on its detail page, and choose the narrowest rule, which is the torrent's hash (§20.2). The rule is a filter: the item is removed from the index and stays out when the crawler sees it again, so a removed item doesn't come back on the next crawl.
- **Copyright notices (United States).** An index is an "information location tool" under 17 U.S.C. §512(d). The safe harbour there is available only to a service provider that, among other things, has registered a designated agent with the US Copyright Office (online, $6 per filing, renewed every three years), publishes the agent's contact, acts expeditiously on valid notices, and adopts and reasonably implements a repeat-infringer policy. A disclaimer alone gives none of this. See https://www.copyright.gov/512/ and https://www.copyright.gov/dmca-directory/.
- **Keep a record** of notices received and what you did, without storing the complained-of content.

## 4. Child-exploitation material
Lodestar's child-safety core runs on every source and its floor can't be switched off, but no filter is complete.
- **If you learn of suspected material**, don't download it to check, don't share it, and don't post it anywhere. Block the hash, and report it: in the United States to the NCMEC CyberTipline (https://report.cybertip.org) or law enforcement; elsewhere to your national hotline (https://www.inhope.org lists them).
- **If your service is a "provider"** of an electronic communication or remote computing service under 18 U.S.C. §2258E, you have a duty under §2258A to report apparent violations once you have actual knowledge, and to preserve what you reported. The statute does not require you to monitor or scan proactively. Whether a public Lodestar is a provider depends on what it offers; find out before you go public.
- **Intimate images.** The TAKE IT DOWN Act obliges a "covered platform" to offer a notice-and-removal process and to remove non-consensual intimate imagery within 48 hours of a valid request. An index of metadata may or may not be a covered platform; check against the FTC's guidance (https://www.ftc.gov/business-guidance/resources/complying-take-it-down-act).

## 5. Personal data
A public instance processes personal data: visitors' IP addresses in access logs, searches if you turn the query log on, API keys, and any account you create for others.
- Keep the query log **off** unless you need it, and clear it when you don't.
- Decide how long logs live, and write it down.
- In the European Union and the United Kingdom, the GDPR makes you the controller of that data. In Colorado, the Colorado Privacy Act applies above thresholds of users or revenue, and the state's security-breach law applies regardless: notify affected Colorado residents within 30 days of determining that a breach occurred (https://coag.gov/resources/data-protection-laws/). Other states have their own rules.
- **Minors.** Lodestar is for adults. If your service is likely to be used by minors, Colorado's protections for minors' online data (SB24-041, in force since 2025-10-01) and the federal COPPA rule for under-13s may apply even to a small service.

## 6. Private sources stay private
Items found only through private trackers, Usenet or your own download clients are flagged `private_source` and are never served over Torznab, included in exports or sent into public peer finding. Don't work around this: the trackers' rules usually forbid redistribution, and a public lookup leaks the hash.

## 7. Be accurate about what you offer
Describe your service as what it is: a searchable index of torrent metadata that other people published. Don't advertise it as a way to get films, music, software or books. That is the conduct that decided the cases against isoHunt and LimeWire, and no notice offsets it.
