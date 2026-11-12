# Security policy

Found a security flaw in Lodestar? Report it privately, so it can be fixed before anyone else learns how to use it.

## How to report a vulnerability
1. Open the repository's **Security** tab: <https://github.com/dvortsis/lodestar-v2/security>.
2. Select **Report a vulnerability**. This is GitHub's private vulnerability reporting: only you and the maintainers can see the report. You need a (free) GitHub account to use it.
3. Describe the flaw, the version or commit you found it in, how to reproduce it, and what an attacker could do with it.

That button is the only reporting channel. There is no security email address.

**Don't report a vulnerability in a public issue, pull request, discussion or comment.** The repository is public, so anything posted there is disclosed at once.

## What to include, and what never to send
- **Do include:** the Lodestar version (`lodestar --version`, the Docker image tag, or the version in Windows' installed apps), how you run it (Docker, Unraid or Windows), the steps to reproduce, and a harmless synthetic example that shows the problem.
- **Never send:** real suspected illegal material, or anything that identifies it (files, screenshots, magnet links, info hashes, torrent names or descriptions). This applies to flaws in the content filters too: show the bypass with made-up names.
- **Never send** your setup token, password, API keys, session cookies, or logs that contain them or torrent details.

## What counts as a security report
**In scope:** the code in this repository and what is built from it:
- the `lodestar` binary and its web UI;
- the Docker image, the Unraid template and the Windows installer;
- the update engine, including how it checks signatures;
- the cluster (worker mode) and the companion manager;
- the GitHub Actions workflows in `.github/workflows/`;
- the project's deployed roadmap dashboard (`tools/status-connector`).

Examples: getting past the login or the setup token, reading or changing data without permission, getting past the child-safety core, a crash or runaway memory or CPU use caused by network input or a search query, a leak of credentials or torrent details into logs, and a way to make the updater install something that isn't signed.

**Not a security report; send these elsewhere:**
- **Reports of suspected illegal content on someone's instance** go to that instance's operator, and child sexual exploitation to the NCMEC CyberTipline or your local authority. See section 9 of the [Legal Notice](docs/legal/legal-notice.md). The maintainers don't operate instances.
- **A flaw in a third-party program** that Lodestar talks to or installs as a companion: report it to that program's maintainers. If Lodestar's use of it makes the flaw worse, that part is in scope.
- **Ordinary bugs and questions:** open an [issue](https://github.com/dvortsis/lodestar-v2/issues).

## Testing rules
- Test only against a Lodestar instance you run yourself. Never test against someone else's instance.
- Don't download, share or index suspected illegal material to test anything.
- Don't run tests that flood the public DHT, trackers or other people's servers.

## What happens after you report
1. The maintainers read the report in its private advisory and reply there.
2. The flaw is rated, as [the review method](docs/security/v1/README.md#3-severity) describes: **blocking** (an attacker can use it now), **important** (it needs extra conditions, or a safeguard is missing) or **polish** (hardening with no plausible way to exploit it).
3. While a blocking or important flaw is unfixed, its details stay in the private advisory. The public repository mentions it only by an id and its severity.
   A report rated polish may be fixed and discussed in public. You'll be told first.
4. The fix is made in a pull request that describes the change without showing how to exploit the flaw.
5. Once a release with the fix is published, the advisory is published too, with credit to you unless you ask otherwise.

## Supported versions
Security fixes go into the next release. Only the latest release gets them, so keep Lodestar up to date. The [user guide](docs/user/) covers installing on Docker, Unraid and Windows.

## How Lodestar is reviewed
Every change is reviewed before it merges, and changes that touch authentication, credentials, untrusted input, the content filters, the cluster or companions also get a dedicated security review. Before 1.0, the whole system is reviewed surface by surface and each fuzz target runs for at least an hour. [docs/security/v1/README.md](docs/security/v1/README.md) describes that review and how its findings are handled.
