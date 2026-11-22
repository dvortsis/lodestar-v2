// UI check (review 02 §3c): every route, built from the pseudo-locale catalogue (messages ~35 %
// longer, accented, wrapped in ⟦ ⟧; scripts/pseudo-locale.mjs), at a phone and a desktop width.
// It fails on:
//   - a page wider than its viewport (horizontal overflow);
//   - text clipped without an ellipsis (the ⟧ end mark shows where text is cut);
//   - hard-coded English: a visible text, label, title, placeholder or alt that holds a run of
//     plain Latin letters. Every letter of every catalogue message is accented, so a plain run did
//     not come from the catalogue.
// Runs only in the `pseudo` project, against its own build, nightly and on manual dispatch.
// Waits are conditions with a 10 s deadline; no fixed sleeps.
import type { Page } from '@playwright/test';
import { freezeClock } from '../snapshot';
import { ROUTES } from '../support/routes';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

const WIDTHS = [
	{ name: 'phone', width: 390, height: 800 },
	{ name: 'desktop', width: 1366, height: 900 }
];

/**
 * Plain-letter runs that are not from the catalogue and are right as they are. Each entry says why.
 * A new one needs a reason, and a real missed string is fixed in the app instead.
 */
const ALLOWED: { why: string; pattern: RegExp }[] = [
	{
		why: 'addresses and environment variable names, which are literal',
		pattern: /https?:\/\/\S+|\b[A-Z][A-Z0-9]*(_[A-Z0-9]+)+\b/g
	},
	{ why: 'the product name, never translated', pattern: /Lodestar/g },
	{
		why: 'protocol and format names, hashes and units that are not words',
		pattern:
			/\b(BEP|DHT|SHA|MSE|uTP|TCP|UDP|IPv[46]|URL|URI|RSS|API|JSON|CSV|NZB|SSE|TLS|HTTPS?|DNS|ID|OK|GiB|MiB|KiB|TiB|GB|MB|KB|TB|B|k|M|G|ms|s|h|d|min|px|x|e|a|f|c|b)\b/g
	},
	{ why: 'hexadecimal (info hashes, ids, colours)', pattern: /\b[0-9a-f]{6,}\b/gi },
	{
		why: 'times and counts are formatted by Intl for the locale, not by the catalogue',
		pattern:
			/\b(ago|in|now|yesterday|tomorrow|today|am|pm|AM|PM|UTC|Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec|Mon|Tue|Wed|Thu|Fri|Sat|Sun|seconds?|minutes?|hours?|days?|weeks?|months?|years?)\b/g
	},
	{
		why: 'fixture data: torrent names, trackers, hosts and saved-search names come from the mock',
		pattern:
			/\b(The|Lantern|Keeper|Raw|Name|Show|Title|GRP|Ubuntu|Debian|Fedora|desktop|netinst|workstation|releases|WEB|DL|WEBRip|BluRay|BDRip|x264|x265|HEVC|AAC|FLAC|MKV|MP4|ISO|S\d+E\d+|1080p|720p|2160p|qBittorrent|SABnzbd|NZBGet|bitmagnet|localhost|example|test|tracker|udp|http|https)\b/gi
	}
];

/**
 * KNOWN GAPS: hard-coded English found by this check that the app still has. Each is a real
 * defect, listed here so the nightly run reports new ones, with the follow-up that removes it.
 * Delete an entry when its fix lands (the check then guards it).
 */
const KNOWN_GAPS: { gap: string; pattern: RegExp }[] = [
	{ gap: 'the relative-time formatter answers "just now" in English', pattern: /\bjust\s+now\b/g },
	{ gap: 'the Insights time KPIs print the unit "hr" in English', pattern: /≈\s*[\d.]+\s*hr\b/g },
	{
		gap: 'Insights annotations print a raw setting value ("aggressive") inside a translated sentence',
		pattern: /\baggressive\b/g
	}
];

interface Finding {
	kind: 'english' | 'clipped' | 'overflow';
	where: string;
	text: string;
}

/** The texts a person or a screen reader gets from the page, with where each one is. */
async function collect(page: Page): Promise<Finding[]> {
	// Known gaps go first: a broader allowance would otherwise eat part of their text.
	const allowed = [...KNOWN_GAPS, ...ALLOWED].map((a) => ({
		source: a.pattern.source,
		flags: a.pattern.flags
	}));
	return page.evaluate((allowedSources) => {
		const allow = allowedSources.map((s) => new RegExp(s.source, s.flags));
		const found: Finding[] = [];
		const where = (el: Element) =>
			el.tagName.toLowerCase() +
			(el.getAttribute('data-testid') ? `[data-testid=${el.getAttribute('data-testid')}]` : '') +
			(el.getAttribute('role') ? `[role=${el.getAttribute('role')}]` : '');
		const english = (text: string) => {
			let rest = text;
			for (const re of allow) rest = rest.replace(re, ' ');
			return /[A-Za-z]{2,}/.test(rest);
		};
		// The boot shell (src/app.html) is static English by design: it paints before the app loads and
		// the app drops it for any other locale. Its copy is checked by boot-shell.routes.test.ts.
		// The legal notice is the server's English document, shown as it is, not a catalogue message.
		const inBoot = (el: Element) =>
			!!el.closest('#boot, #boot-header, [data-testid="legal-notice"]');
		const visible = (el: Element) => {
			if (inBoot(el)) return false;
			for (let e: Element | null = el; e; e = e.parentElement) {
				const cs = getComputedStyle(e);
				if (cs.display === 'none' || cs.visibility === 'hidden') return false;
			}
			return true;
		};

		// Text nodes (screen-reader-only text counts: a person with a screen reader hears it).
		const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
		for (let n = walker.nextNode(); n; n = walker.nextNode()) {
			const parent = n.parentElement;
			const text = (n.textContent ?? '').trim();
			if (!parent || !text || /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE)$/.test(parent.tagName)) continue;
			if (!visible(parent)) continue;
			if (english(text)) found.push({ kind: 'english', where: where(parent), text });
		}
		// Attributes that are read or shown.
		for (const el of document.body.querySelectorAll('[aria-label],[title],[placeholder],[alt]')) {
			if (!visible(el)) continue;
			for (const attr of ['aria-label', 'title', 'placeholder', 'alt']) {
				const value = el.getAttribute(attr)?.trim();
				if (value && english(value))
					found.push({ kind: 'english', where: `${where(el)}@${attr}`, text: value });
			}
		}
		// Clipped text: cut off at its own edge with no ellipsis to say so.
		for (const el of document.body.querySelectorAll('*')) {
			const cs = getComputedStyle(el);
			if (!/^(hidden|clip)$/.test(cs.overflowX) || cs.textOverflow === 'ellipsis') continue;
			// Screen-reader-only boxes (1 px, clipped on purpose) are not clipped text.
			if (cs.display === 'none') continue;
			if (cs.width === '1px' || cs.height === '1px' || cs.clipPath === 'inset(50%)') continue;
			if (el.scrollWidth <= el.clientWidth + 1) continue;
			if (!(el.textContent ?? '').includes('⟦')) continue;
			found.push({
				kind: 'clipped',
				where: where(el),
				text: (el.textContent ?? '').trim().slice(0, 80)
			});
		}
		if (document.documentElement.scrollWidth > window.innerWidth)
			found.push({
				kind: 'overflow',
				where: 'html',
				text: `${document.documentElement.scrollWidth} px wide in ${window.innerWidth} px`
			});
		return found;
	}, allowed);
}

for (const size of WIDTHS) {
	test.describe(`pseudo-locale, ${size.name}`, () => {
		test.use({ viewport: { width: size.width, height: size.height } });

		for (const route of ROUTES) {
			test(`${route.name}: fits, is not clipped, has no hard-coded English`, async ({
				page,
				go,
				mock
			}) => {
				await freezeClock(page);
				route.mock?.(mock);
				await go(route.path);
				// The page shows pseudo text: proves this is the pseudo build, and the page has rendered.
				await expect(page.locator('main').first()).toContainText('⟦');
				const findings = await collect(page);
				expect(findings.map((f) => `${f.kind} ${f.where}: ${f.text}`)).toEqual([]);
			});
		}
	});
}
