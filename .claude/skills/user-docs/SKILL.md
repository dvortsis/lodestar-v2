---
name: user-docs
description: Rules for writing Lodestar's user-facing documentation for people who can install an app from the Unraid app store but may not know how to set it up, how Docker or Windows security work, or good security practice. Guide them plainly and briefly, with safe recommendations, screenshots and diagrams. Use it whenever you write or edit the README, anything under docs/user/, docs/operations.md, release notes, in-app "Learn more" or help pages, or any help text longer than a sentence. Not for developer or contract docs (architecture.md, build-plan.md, design-tokens.md, metrics.md, runner/).
---

# Writing Lodestar's user documentation

The owner decided this on 2026-10-22 (`docs/owner-decisions.md` §2.14): **write for someone who can install an app from the Unraid app store but may not know how to set it up, and guide them to a safe setup plainly and briefly.** Show it with screenshots and diagrams wherever you can.

## 1. Who you're writing for
**Someone who can install an app from the Unraid app store (Community Applications), but may not know how to set it up.**

- **They can:**
  - find and install an app from Unraid's Community Applications, or run the Windows installer;
  - open a web page by its address;
  - copy and paste a command when told exactly where.
- **They may not know:**
  - **What the setup fields mean,** such as the paths, the ports, the appdata folder, or why a field matters.
  - **How Docker actually works:**
    - that a container is thrown away and rebuilt, and only mapped folders survive;
    - what a port mapping or a network mode does;
    - why `/mnt/user` and `/mnt/cache` behave differently.
  - **Windows' security features and limits:**
    - firewall prompts, SmartScreen and Defender warnings;
    - services and the accounts they run as;
    - why real-time scanning slows a database down.
  - **The networking:** what the DHT is, what port forwarding does, why UDP matters, what `0.0.0.0` means.
  - **Good security and safety practice:**
    - strong passwords;
    - not exposing the web UI to the internet;
    - what a VPN does and doesn't hide;
    - keeping API keys private;
    - backups and updates.
- **They want** to get it working **safely**, understand enough to make the right choice at each step, and not break anything.

**Guide them, plainly.** Write as a calm, matter-of-fact guide:
- say what to do, why in a sentence, and what's safe;
- don't lecture, don't reassure ("Don't worry!") and don't pad.

Test each page by asking whether this reader could follow it **without opening another website** and end up with a **safe** setup. If a step needs outside knowledge, explain it briefly on the page, or link to one of our pages that does.

## 2. Writing rules
- **Explain enough to choose well, and no more.** Give the reason in one sentence ("Lodestar needs to receive messages from other computers, so the DHT port must be open"). Longer background goes in a collapsed block or on an explanation page:
  ```
  <details><summary>Why does this matter?</summary>

  …

  </details>
  ```
- **Be brief:**
  - Cut words that don't help the reader act or decide.
  - One idea per sentence; aim for under 20 words.
  - Steps start with the verb ("Open…", "Set…", "Click…").
  - If a paragraph runs past four lines, ask whether it's a list, a step, or background that belongs in a collapsed block.
- **Define every technical term the first time it appears**, in plain words, and link the glossary (`docs/user/glossary.md`, which builds on `docs/ui-glossary.md`). Example: "the **DHT** (the shared address book that BitTorrent computers use to find each other)".
- **Use the app's exact words.** Buttons, screens and fields are in **bold**, exactly as the UI writes them (`docs/ui-glossary.md`, §10.14). Give paths through the UI as **Settings → Network & VPN → DHT port**.
- **One action per numbered step.** After any step that changes something, say what you should see ("The status pill turns to **Crawling**").
- **Commands:**
  - Commands are in code blocks, ready to copy, **one command per block**.
  - Mark anything to replace as `<like-this>`, and explain each one under the block ("`<your-server-ip>`: the address you open Lodestar at, for example `192.168.1.20`").
  - Say which terminal to use, and how to open it on that platform, the first time a page uses one.
- **Platform differences** go in clearly headed sections, in this order: **Unraid**, **Docker**, **Windows**, **Linux**. Never say "the usual way".
- **Warn before a risky step, not after.** Use a callout with what could go wrong and how to undo it:
  ```
  > [!WARNING]
  > Changing this port disconnects Lodestar from the DHT until it's open in your router too. To undo, set it back to 6881.
  ```
  GitHub renders `> [!NOTE]`, `> [!TIP]`, `> [!IMPORTANT]`, `> [!WARNING]` and `> [!CAUTION]`. Use NOTE and TIP sparingly, and WARNING or CAUTION only for real risk.
- **Plain, matter-of-fact English:**
  - active voice, addressing the reader as "you";
  - about a US grade 8 reading level;
  - no "simply", "just", "obviously", "easy" or "of course";
  - no exclamation marks, and no filler ("In this guide we will…", "It's worth noting that…").
- **Realistic, safe examples:**
  - private addresses (`192.168.1.20`) and `example.com`;
  - fixture torrent names from the test data (`Big.Buck.Bunny.2008.1080p`);
  - **never** real third-party torrent names, real IPs, tokens, API keys or passwords, even fake-looking ones that could be real.
- **Be honest about limits:** say what Lodestar can't do or can't know (architecture §1, "Hard truths"), and how long things take ("The first results appear within a few minutes; a useful index takes a few days").

## 2b. Safety and security guidance
Assume the reader doesn't know good practice, and build it into the steps rather than into a lecture at the end.
- **Every choice has a recommended answer.** Mark it **(recommended)** and say why in one line. When the safe choice is the default, say so ("Leave this as it is").
- **Name unsafe choices plainly.** Say what can go wrong and what to do instead, without drama. For example:

  > [!CAUTION]
  > Don't forward port 8090 on your router. It would put Lodestar's web page on the internet. To reach it from outside your home, use a VPN such as Tailscale or WireGuard instead.

- **Cover these wherever they come up:**
  - **The web UI stays on your home network.** Never port-forward 8090. For remote access, use a VPN, or a reverse proxy with HTTPS (advanced).
  - **The admin password:** use a long, unique one from a password manager. Keep the setup token private, because it's only valid until setup is done.
  - **API keys and feed tokens** work like passwords: one per app, and regenerate any that leaks.
  - **Only the DHT port is forwarded**, and only when the reader chooses to.
  - **What a VPN hides and what it doesn't.** Crawling the DHT shows your IP to other peers; a VPN hides it. Lodestar never downloads torrent content.
  - **Unraid:**
    - appdata goes on the cache pool (`/mnt/cache/appdata/lodestar`), not `/mnt/user`;
    - the paths the template should map, and why;
    - which template fields to leave alone.
  - **Windows:**
    - what the SmartScreen and firewall prompts are and which answer to give;
    - the optional Defender exclusion, what it trades away and when to take it;
    - why Lodestar runs as a service.
  - **Backups** (where they are and how to restore), **updates** (§12.2), and **the content filters'** defaults and what changing them means.
- **Never teach an unsafe shortcut**, even as an aside: no disabling the firewall, no running as administrator "to make it work", no `chmod 777`, no turning off sign-in.

## 3. Page structure
Use the type of page that fits the reader's goal. Don't mix them on one page.
- **Get started** (tutorials): one path from nothing to a working result, for example "Install Lodestar on Unraid".
- **How-to guides:** one task each, for example "Open the DHT port on your router", "Send results to qBittorrent", "Turn on automatic updates".
- **Explanations:** how something works and why, for example "How Lodestar finds torrents", "What the content filters do".
- **Reference:** settings, the search syntax, environment variables. Every entry gets a one-line plain explanation, not only a type.

Every get-started and how-to page has these parts, in this order:
1. **A title that names the outcome** ("Open the DHT port", not "Port configuration").
2. **What this does**, in one or two sentences, with why you'd want it.
3. **Before you start:** what you need, about how long it takes, and anything to back up first.
4. **The steps:** numbered, with screenshots (§4) and a check after each change.
5. **Check that it worked:** what success looks like, and where in the app to see it.
6. **If something went wrong:** the common failures, each as its symptom, then the likely cause, then the fix. Link the live log (`/logs`) and Download diagnostics where they help.
7. **What's next:** one to three links.

## 4. Screenshots
- **Show it whenever a step happens in Lodestar's UI.** One screenshot for each screen the reader has to find something on. The step's text still has to work without the image.
- **Capture them automatically from the real app:**
  - with the Playwright docs capture in `web/tests/docs/` (slice `S-doc-shots`);
  - using the fixture dataset and Riverstone light;
  - at a 1280 × 800 viewport, or 390 × 844 when the page is about phones;
  - cropped to the region that matters.

  The capture script names each image after its page and step (`docs/user/images/<page>/<nn>-<step>.png`), so a UI change regenerates the docs' images in the same PR.
- **Point at things in the capture, never by drawing on images by hand.** The script adds an outline around the element the step refers to, using the focus-ring token, and nothing else: no arrows, no red boxes.
- **Screens Lodestar doesn't own** (Unraid, Docker Desktop, Windows, a router admin page) are captured by hand.
  - Before committing one, check that it shows no personal data: hostnames, IPs other than private documentation ones, serial numbers, usernames, tokens.
  - Note the product and version under it ("Unraid 7.1"), because these screens change without warning.
- **Every image has alt text** that says what the reader needs from it ("The Network & VPN screen, with the DHT port field set to 6881"), not "screenshot".
- **Size:** each image is at most 200 KB after optimisation (`oxipng` or sharp's PNG compression). Dark-theme variants only where the difference matters, with `<picture>` and `prefers-color-scheme`.
- **Never** include real torrent names, logs written with torrent details on, secrets or setup tokens in a screenshot.

## 5. Diagrams
- **Draw a diagram when the reader has to understand something that moves or connects:**
  - how a torrent goes from the DHT to search (Discover → Fetch → Understand → Index → Serve);
  - where traffic flows through a VPN;
  - how a port reaches Lodestar through the router and Docker;
  - what happens during an update and its rollback.
- **Use Mermaid** in Markdown, which GitHub renders, so the diagram is text, can be reviewed and stays in sync:
  - flowcharts for processes and decisions;
  - sequence diagrams for "what talks to what";
  - at most about 12 boxes. Split anything bigger.
  - Don't hard-code colours, so it works in both GitHub themes.
- **Every diagram is followed by a sentence or short list that says the same thing in words,** for screen readers and for anyone the diagram didn't help.
- **In-app help** uses static SVG in the spirit of the `infographic-design` skill (`dvortsis/skills`), with a text equivalent, as the onboarding explainer already does (§10.10.10).

## 6. Before you finish: checklist
- [ ] Could the reader above follow it without another website, and end up with a safe setup?
- [ ] Every choice shows the recommended answer, and every unsafe option is named, with the safe alternative.
- [ ] Brief: no filler, background collapsed or linked, sentences mostly under 20 words.
- [ ] Every term defined or linked on first use, and every placeholder explained.
- [ ] A one-line why before each step that needs one, and a check after each change.
- [ ] Platform sections cover Unraid, Docker, Windows and Linux where the steps differ.
- [ ] Risky steps are warned about before they happen, with the undo.
- [ ] A screenshot for each Lodestar screen the steps use, with alt text and no personal data.
- [ ] A diagram wherever something flows or connects, followed by its text version.
- [ ] "If something went wrong" covers the failures a newcomer is likely to hit.
- [ ] UI labels match `docs/ui-glossary.md` exactly.
- [ ] Nothing describes planned behaviour as if it exists. Mark it **(planned)**.
