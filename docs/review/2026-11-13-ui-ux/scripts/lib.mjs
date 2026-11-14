// Shared helpers for the UI review harness. Playwright and axe resolve from web/node_modules.
import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync } from 'node:fs';
const require = createRequire(new URL('../../../../web/', import.meta.url));
export const { chromium } = require('playwright');
export const AxeBuilder = require('@axe-core/playwright').default;

export const BASE = process.env.BASE ?? 'http://127.0.0.1:8291';
export const PASSWORD = process.env.PASSWORD ?? 'correct horse battery';
export const OUT = process.env.OUT ?? new URL('./out', import.meta.url).pathname;
mkdirSync(OUT, { recursive: true });

export async function launch() {
	return chromium.launch({ executablePath: process.env.PW_CHROMIUM_PATH ?? '/opt/pw-browsers/chromium', args: ['--no-sandbox'] });
}

let cookies;
export async function signIn(browser) {
	if (cookies) return cookies;
	const ctx = await browser.newContext({ baseURL: BASE });
	const res = await ctx.request.post('/api/v1/login', { data: { password: PASSWORD } });
	if (!res.ok()) throw new Error('login failed ' + res.status() + ' ' + (await res.text()));
	cookies = (await ctx.storageState()).cookies;
	await ctx.close();
	return cookies;
}

export async function context(browser, { width, height = 900, mode = 'light', coarse = false, signedIn = true, reducedMotion, locale, theme } = {}) {
	const ctx = await browser.newContext({
		baseURL: BASE,
		viewport: { width, height },
		deviceScaleFactor: 1,
		hasTouch: coarse,
		isMobile: coarse,
		colorScheme: mode,
		reducedMotion: reducedMotion ?? 'no-preference',
		locale: locale ?? 'en-US'
	});
	if (signedIn) await ctx.addCookies(await signIn(browser));
	const stored = theme ?? { mode };
	await ctx.addInitScript((s) => {
		try {
			localStorage.setItem('lodestar.theme', JSON.stringify(s));
		} catch {}
	}, stored);
	return ctx;
}

export function watch(page) {
	const log = { console: [], pageErrors: [], failed: [], http: [] };
	page.on('console', (m) => {
		if (m.type() === 'error' || m.type() === 'warning') log.console.push(`${m.type()}: ${m.text()}`.slice(0, 400));
	});
	page.on('pageerror', (e) => log.pageErrors.push(String(e).slice(0, 400)));
	page.on('requestfailed', (r) => log.failed.push(`${r.method()} ${r.url()} ${r.failure()?.errorText}`));
	page.on('response', (r) => {
		if (r.status() >= 400) log.http.push(`${r.status()} ${r.request().method()} ${r.url().replace(BASE, '')}`);
	});
	return log;
}

export async function settle(page, ms = 600) {
	try {
		await page.waitForLoadState('networkidle', { timeout: 2500 });
	} catch {}
	await page.waitForTimeout(ms);
}

// Wake the lazy chrome (it loads on the first interaction).
export async function wake(page) {
	await page.mouse.move(5, 5);
	await page.dispatchEvent('body', 'pointerdown').catch(() => {});
	await settle(page, 500);
}

export async function probe(page) {
	return page.evaluate(() => {
		const vw = window.innerWidth;
		const de = document.documentElement;
		const sel = (el) => {
			const parts = [];
			let e = el;
			for (let i = 0; e && e !== document.body && i < 4; i++, e = e.parentElement) {
				let s = e.tagName.toLowerCase();
				if (e.id) s += '#' + e.id;
				const t = e.getAttribute('data-testid');
				if (t) s += `[testid=${t}]`;
				else if (typeof e.className === 'string' && e.className) s += '.' + e.className.split(/\s+/).slice(0, 2).join('.');
				parts.unshift(s);
			}
			return parts.join(' > ').slice(0, 200);
		};
		const visible = (el) => {
			const r = el.getBoundingClientRect();
			if (r.width < 1 || r.height < 1) return false;
			const cs = getComputedStyle(el);
			if (cs.visibility === 'hidden' || cs.display === 'none' || +cs.opacity === 0) return false;
			// sr-only
			if (cs.position === 'absolute' && r.width <= 1 && r.height <= 1) return false;
			if (cs.clipPath && cs.clipPath !== 'none' && r.width <= 2) return false;
			return true;
		};
		const scrollParentX = (el) => {
			for (let e = el.parentElement; e; e = e.parentElement) {
				const cs = getComputedStyle(e);
				if (/(auto|scroll|hidden|clip)/.test(cs.overflowX) && e !== document.body && e !== de) return e;
			}
			return null;
		};
		const out = {};
		out.url = location.pathname + location.search;
		out.title = document.title;
		out.hOverflow = de.scrollWidth - vw;
		out.bodyH = de.scrollHeight;
		// Elements that stick out of the viewport and are not inside a clipped/scrolling box.
		const poke = [];
		for (const el of document.querySelectorAll('body *')) {
			if (!visible(el)) continue;
			const r = el.getBoundingClientRect();
			if (r.right > vw + 1 || r.left < -1) {
				if (scrollParentX(el)) continue;
				const cs = getComputedStyle(el);
				if (cs.position === 'fixed' && (r.left >= vw || r.right <= 0)) continue;
				poke.push(`${sel(el)} [${Math.round(r.left)}..${Math.round(r.right)}]`);
				if (poke.length >= 8) break;
			}
		}
		out.poke = poke;
		// Interactive targets.
		const inter = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=switch],[role=tab],[role=menuitem],[role=option],[role=radio]';
		const small = [];
		const unnamed = [];
		let nInter = 0;
		for (const el of document.querySelectorAll(inter)) {
			if (!visible(el)) continue;
			nInter++;
			const r = el.getBoundingClientRect();
			// inline links inside text are exempt from target size
			const inlineLink = el.tagName === 'A' && getComputedStyle(el).display === 'inline';
			if (!inlineLink && (r.width < 24 || r.height < 24)) small.push(`${sel(el)} ${Math.round(r.width)}x${Math.round(r.height)}`);
			const name = (el.getAttribute('aria-label') || el.getAttribute('aria-labelledby') || el.textContent || el.getAttribute('title') || el.getAttribute('placeholder') || (el.labels && el.labels.length ? 'l' : '') || el.getAttribute('value') || '').trim();
			if (!name && !el.querySelector('img[alt]')) unnamed.push(sel(el));
		}
		out.nInteractive = nInter;
		out.small = small.slice(0, 25);
		out.smallCount = small.length;
		out.unnamed = unnamed.slice(0, 10);
		// Text sizes and clipped text.
		const sizes = {};
		const tiny = [];
		const clipped = [];
		const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
		const seen = new Set();
		while (walker.nextNode()) {
			const n = walker.currentNode;
			if (!n.nodeValue.trim()) continue;
			const el = n.parentElement;
			if (!el || seen.has(el) || !visible(el)) continue;
			seen.add(el);
			const cs = getComputedStyle(el);
			const fs = parseFloat(cs.fontSize);
			const key = `${fs}/${cs.lineHeight}/${cs.fontWeight}`;
			sizes[key] = (sizes[key] || 0) + 1;
			if (fs < 12) tiny.push(`${sel(el)} ${fs}px "${n.nodeValue.trim().slice(0, 30)}"`);
			// clipped: nearest block with overflow hidden where scrollWidth > clientWidth
			for (let e = el, i = 0; e && i < 3; e = e.parentElement, i++) {
				const c = getComputedStyle(e);
				if ((c.overflowX === 'hidden' || c.overflowX === 'clip') && e.scrollWidth > e.clientWidth + 1 && e.clientWidth > 0) {
					const hasTitle = e.getAttribute('title') || el.getAttribute('title');
					clipped.push(`${sel(e)} "${(e.textContent || '').trim().slice(0, 50)}" ${hasTitle ? '(title)' : ''} ${c.textOverflow}`);
					break;
				}
			}
		}
		out.typeScale = sizes;
		out.tiny = tiny.slice(0, 10);
		out.clipped = [...new Set(clipped)].slice(0, 15);
		out.clippedCount = clipped.length;
		// Headings.
		out.headings = [...document.querySelectorAll('h1,h2,h3,h4,h5,h6')].filter(visible).map((h) => `${h.tagName}:${(h.textContent || '').trim().slice(0, 50)}`).slice(0, 40);
		out.landmarks = [...document.querySelectorAll('header,nav,main,aside,footer,[role=search],[role=region],[role=complementary],section[aria-label],section[aria-labelledby],form[aria-label]')].filter(visible).map((l) => `${l.tagName.toLowerCase()}${l.getAttribute('role') ? '[' + l.getAttribute('role') + ']' : ''}:${l.getAttribute('aria-label') || l.getAttribute('aria-labelledby') || ''}`).slice(0, 30);
		// Images.
		out.imgNoAlt = [...document.querySelectorAll('img')].filter((i) => !i.hasAttribute('alt')).map(sel);
		out.imgNoDims = [...document.querySelectorAll('img')].filter((i) => !i.getAttribute('width') || !i.getAttribute('height')).map(sel);
		// Radii in use (for the geometry check).
		const radii = {};
		for (const el of document.querySelectorAll('body *')) {
			if (!visible(el)) continue;
			const cs = getComputedStyle(el);
			const r = cs.borderTopLeftRadius;
			if (r && r !== '0px') {
				const hasBox = cs.backgroundColor !== 'rgba(0, 0, 0, 0)' || cs.borderTopWidth !== '0px';
				if (hasBox) radii[r] = (radii[r] || 0) + 1;
			}
		}
		out.radii = radii;
		out.lang = de.lang;
		out.theme = de.getAttribute('data-theme');
		return out;
	});
}

export async function axe(page, opts = {}) {
	let b = new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa', 'best-practice']);
	if (opts.include) b = b.include(opts.include);
	const res = await b.analyze();
	return res.violations.map((v) => ({
		id: v.id,
		impact: v.impact,
		help: v.help,
		n: v.nodes.length,
		targets: v.nodes.slice(0, 4).map((n) => `${n.target.join(' ')} :: ${(n.failureSummary || '').split('\n').slice(1, 2).join(' ').trim().slice(0, 160)}`)
	}));
}

export function save(name, data) {
	writeFileSync(`${OUT}/${name}.json`, JSON.stringify(data, null, 1));
}

export async function shot(page, name, full = true) {
	await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: full, animations: 'disabled' });
}
