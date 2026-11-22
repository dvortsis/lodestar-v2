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
import { plainEnglish } from '../../scripts/pseudo-locale.mjs';
import { freezeClock } from '../snapshot';
import { ROUTES } from '../support/routes';
import { expect as baseExpect, test } from '../support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

const WIDTHS = [
	{ name: 'phone', width: 390, height: 800 },
	{ name: 'desktop', width: 1366, height: 900 }
];

interface Finding {
	kind: 'english' | 'clipped' | 'overflow';
	where: string;
	text: string;
}

interface Text {
	where: string;
	text: string;
	tag: string;
	testid: string | null;
	time: boolean;
}

/**
 * What a person or a screen reader gets from the page: every visible text and label, with the
 * element it is on (judged for hard-coded English in Node, where `plainEnglish` and its allowances
 * live and are unit-tested), and the clipped text and overflow found in the page itself.
 */
async function collect(page: Page): Promise<{ texts: Text[]; found: Finding[] }> {
	return page.evaluate(() => {
		const texts: Text[] = [];
		const found: Finding[] = [];
		const where = (el: Element) =>
			el.tagName.toLowerCase() +
			(el.getAttribute('data-testid') ? `[data-testid=${el.getAttribute('data-testid')}]` : '') +
			(el.getAttribute('role') ? `[role=${el.getAttribute('role')}]` : '');
		const add = (el: Element, value: string, suffix = '') =>
			texts.push({
				where: where(el) + suffix,
				text: value,
				tag: el.tagName.toLowerCase(),
				testid: el.getAttribute('data-testid'),
				time: !!el.closest('time')
			});
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
			const value = (n.textContent ?? '').trim();
			if (!parent || !value || /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE)$/.test(parent.tagName)) continue;
			if (!visible(parent) || inLegal(parent)) continue;
			add(parent, value);
		}
		// Attributes that are read or shown.
		for (const el of document.body.querySelectorAll('[aria-label],[title],[placeholder],[alt]')) {
			if (!visible(el)) continue;
			for (const attr of ['aria-label', 'title', 'placeholder', 'alt']) {
				const value = el.getAttribute(attr)?.trim();
				if (value) add(el, value, `@${attr}`);
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
		return { texts, found };
	});
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
				const { texts, found } = await collect(page);
				const english: Finding[] = texts
					.filter((t) =>
						plainEnglish(t.text, { route: route.name, tag: t.tag, testid: t.testid, time: t.time })
					)
					.map((t) => ({ kind: 'english', where: t.where, text: t.text }));
				const findings = [...english, ...found];
				expect(findings.map((f) => `${f.kind} ${f.where}: ${f.text}`)).toEqual([]);
			});
		}
	});
}
