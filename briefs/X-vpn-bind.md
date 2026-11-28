# X-vpn-bind (Opus; runner brief 2026-11-28)

**Branch:** `slice/X-vpn-bind`. **PR title:** `X-vpn-bind: An optional setting to crawl only through one network interface, pausing with VPN down when it goes`.

## Why
The owner (owner-decisions §2.85, D8): "The VPN should be optional and work with any VPN provider, along with system-wide VPNs or in other containers." Plan `docs/plan/2026-11-26-fastest-1.0-proposal.md` §4.6. This is in 1.0. The guide's VPN section (D-vpn-guide) waits on this slice.

## The behaviour
- A new setting, "Crawl only through this network interface". Its default is **Any**, which is today's behaviour, exactly. It can also name an interface such as `wg0` or `tun0`. It works on Linux and Windows.
- With an interface named, every socket the crawl and fetch use is bound to it: DHT (UDP), peer-wire TCP, uTP and tracker/scrape traffic if the crawler makes any. Find every socket the crawler opens, not just the obvious ones, and list them in the decisions file.
- **If the interface is missing at start or disappears later, crawling pauses with a "VPN down" status.** It never falls back to the normal route. When the interface comes back, crawling resumes.
- It is one leak guard that works with any VPN: system-wide (including Unraid's built-in WireGuard), another container, or gluetun.
- It is not a replacement for gluetun's kill switch. The guide explains the routes (D-vpn-guide).

## Step 0: before writing code
Send the runner the following and wait for the answer:
1. Your proposed owns. Likely candidates are `crates/lodestar/src/config.rs`, the socket set-up in `crates/dht`, `crates/peer-wire` and `crates/utp`, the settings API and DTO, and the settings screen in `web/`.
2. How you'll bind on each OS. On Linux that is probably `SO_BINDTODEVICE` through `socket2`, and what it needs inside a container. On Windows it is probably `IP_UNICAST_IF`, or binding to the interface's address.
3. How "the interface went away" is detected.
4. An estimated size.

If the estimate is over about 1,500 changed lines, propose a split. One option is the core binding, the pause and the API first, with the settings screen as a second slice. The runner decides.

## Rules
- **No `unsafe`.** The workspace forbids it. Use `socket2` or the existing helpers.
- **Fail closed.** With an interface set, no crawl or fetch packet may leave by another route at any point: at start, during a reconnect, or when the interface flaps.
  - Test this. At minimum, use a unit-level check that every socket constructor goes through the bind. On Linux, add a test with a dummy or veth interface if CI allows. Otherwise use a test seam and say plainly what CI proves and what it doesn't.
- **Validate the setting.** The interface name is user input: check its length and charset, and never pass it to a shell.
- **Untrusted network input and a privacy control:** run `/security-review` on the diff.
- **Any** must leave today's behaviour and today's performance unchanged. The crawl rounds on the host measure this.
- **No fixed sleeps in tests.** Use condition waits with deadlines of at least 10 s.
- **No cloud calls.** Don't use AWS tools, even ones that are announced. Never touch the owner's machines.
- **Text and commits:**
  - Metrics follow `docs/metrics.md`.
  - UI text follows `docs/ui-glossary.md` and the en and zh message files.
  - No model identifiers in commits, PRs or code. UTC `Z` only.
- Never merge.

## Done
- CI is green on Linux and Windows.
- Self-review per `.claude/skills/steward/SKILL.md`, plus `/security-review`.
- The decisions file `docs/decisions/2026-11-28-X-vpn-bind.md`, with the socket list and what is proven where.
- Send `ready <full sha>` to the runner with the claude-code-remote send_message tool (session_id session_01EYhgh4JX3AmHzKXPjYCoUg).
