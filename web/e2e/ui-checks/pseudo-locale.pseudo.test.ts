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
		why: 'product names of the clients Lodestar talks to',
		pattern: /\b(qBittorrent|SABnzbd|NZBGet|bitmagnet)\b/g
	},
	{
		why: 'protocol and format names, which are not words',
		pattern:
			/\b(BEP|DHT|SHA|MSE|uTP|TCP|UDP|IPv[46]|URL|URI|RSS|API|JSON|CSV|NZB|SSE|TLS|HTTPS?|DNS)\b/g
	},
	{
		why: 'hexadecimal with a digit in it (info hashes, ids), which is not a word',
		pattern: /\b(?=[0-9a-f]*\d)[0-9a-f]{6,}\b/gi
	},
	{
		why: 'byte and time units are formatted by Intl for the locale, not by the catalogue',
		pattern: /\b(GiB|MiB|KiB|TiB|KB|MB|GB|TB|ms|sec|secs|min|hr|hrs)\b/g
	},
	{
		why: 'relative times and dates are formatted by Intl for the locale ("in 5 min", "3 days ago")',
		pattern:
			/\b(in\s+)?\d[\d.,]*\s*(seconds?|minutes?|hours?|days?|weeks?|months?|years?)(\s+ago)?\b|\b(yesterday|tomorrow|today|now)\b|\b(Mon|Tue|Wed|Thu|Fri|Sat|Sun|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\b|\b\d\s?[AP]M\b|\bUTC\b/gi
	},
	// Fixture data comes from the mocks in e2e/support/routes.ts. Each shape is as narrow as the
	// fixture: a real missed word that happens to be "Show" or "Title" must still be caught.
	{
		why: 'fixture: the saved search and torrent name "(The) Lantern Keeper (releases)"',
		pattern: /\b(The )?Lantern Keeper( releases)?\b/g
	},
	{
		why: 'fixture: dotted release names (The.Lantern.Keeper.2160p, Show.S01.1080p.WEB-DL-GRP)',
		pattern: /\b[\w-]+(\.[\w-]+){2,}\b/g
	},
	{ why: 'fixture: the torrent page title and Discover row titles', pattern: /^(Show|Title \d+)$/g }
];

/**
 * KNOWN GAPS: hard-coded English found by this check that the app still has. Each is a real
 * defect, listed here so the nightly run reports new ones, with the follow-up that removes it.
 * Delete an entry when its fix lands (the check then guards it). Gaps go first in the list: a
 * broader allowance would otherwise eat part of their text.
 */
const KNOWN_GAPS: { gap: string; pattern: RegExp }[] = [
	{
		gap: 'relative-time callers do not pass the catalogue\'s "just now" (src/lib/format/relative.ts keeps an English fallback)',
		pattern: /\bjust\s+now\b/g
	},
	{
		gap: 'Insights annotations insert the raw setting value ("aggressive") into a translated sentence (Movement.svelte)',
		// Only inside a pseudo message, so the word is not hidden everywhere else.
		pattern: /(?<=⟦[^⟧]*)\baggressive\b/g
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
		const inBoot = (el: Element) => !!el.closest('#boot, #boot-header');
		// The legal notice's body is the server's English document, shown as it is. Only its text
		// is skipped: the section's own label comes from the catalogue and is checked.
		const inLegal = (el: Element) => !!el.closest('[data-testid="legal-notice"]');
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
			if (!visible(parent) || inLegal(parent)) continue;
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
		// Clipped text: cut off at its own edge with no ellipsis or line clamp to say so, sideways or
		// down. Screen-reader-only boxes (1 px, clipped on purpose) are not clipped text.
		for (const el of document.body.querySelectorAll('*')) {
			const cs = getComputedStyle(el);
			if (cs.display === 'none') continue;
			if (cs.width === '1px' || cs.height === '1px' || cs.clipPath === 'inset(50%)') continue;
			if (!(el.textContent ?? '').includes('⟦')) continue;
			const clip = /^(hidden|clip)$/;
			const sideways =
				clip.test(cs.overflowX) &&
				cs.textOverflow !== 'ellipsis' &&
				el.scrollWidth > el.clientWidth + 1;
			const down =
				clip.test(cs.overflowY) &&
				!/^[1-9]/.test(cs.getPropertyValue('-webkit-line-clamp')) &&
				el.scrollHeight > el.clientHeight + 1;
			if (sideways || down)
				found.push({
					kind: 'clipped',
					where: `${where(el)}${down && !sideways ? ' (down)' : ''}`,
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
