// 200% zoom at 1366 px = a 683 px layout at DPR 2; plus WCAG 1.4.12 text-spacing overrides at 1366.
import { launch, signIn, settle, wake, probe, shot, BASE } from './lib.mjs';
const ROUTES = [['search', '/search?q=lantern'], ['browse', '/search?browse=1'], ['detail', '/t/458dd874810e481cc1691c8e741ee25b49af0079'], ['compare', '/compare?h=458dd874810e481cc1691c8e741ee25b49af0079,340d4ea26fa4cf61f0ee67685bd76dbd83853833'], ['discover', '/discover'], ['alerts', '/alerts'], ['insights', '/insights'], ['ins-fetch', '/insights/fetch'], ['settings', '/settings'], ['s-crawler', '/settings/crawler'], ['s-filters', '/settings/filters'], ['s-security', '/settings/security'], ['s-sources', '/settings/sources'], ['s-logging', '/settings/logging'], ['logs', '/logs']];
const browser = await launch();
for (const [mode, vp, dpr, css] of [['textspacing', { width: 1366, height: 900 }, 1, '* { line-height: 1.5 !important; letter-spacing: 0.12em !important; word-spacing: 0.16em !important; } p { margin-bottom: 2em !important; }']]) {
  const ctx = await browser.newContext({ baseURL: BASE, viewport: vp, deviceScaleFactor: dpr, bypassCSP: !!css });
  await ctx.addCookies(await signIn(browser)); await ctx.addInitScript(() => { try { localStorage.setItem('lodestar.theme', JSON.stringify({ mode: 'light' })); } catch {} });
  const page = await ctx.newPage();
  for (const [n, p] of ROUTES) { try { await page.goto(p); await settle(page); await wake(page); if (css) { await page.addStyleTag({ content: css }); await settle(page, 400); } const r = await probe(page); await shot(page, `f9-${mode}-${n}`, false);
      const cut = await page.evaluate(() => { const out = []; for (const el of document.querySelectorAll('button, a.btn, [role=tab], label, th, .badge, [class*=rr-badge]')) { const r = el.getBoundingClientRect(); if (r.width < 2) continue; if (el.scrollWidth > el.clientWidth + 2 || el.scrollHeight > el.clientHeight + 4) out.push(`${el.tagName.toLowerCase()} "${(el.textContent || '').trim().slice(0, 24)}" ${el.scrollWidth}>${el.clientWidth}w ${el.scrollHeight}>${el.clientHeight}h`); } return out.slice(0, 6); });
      console.log(mode, n, 'hOv', r.hOverflow, 'poke', r.poke.length, r.poke.slice(0, 2), 'clipped', r.clippedCount, 'cutControls', JSON.stringify(cut)); } catch (e) { console.log('FAIL', mode, n, String(e).split('\n')[0].slice(0, 120)); } }
  await ctx.close();
}
await browser.close();
