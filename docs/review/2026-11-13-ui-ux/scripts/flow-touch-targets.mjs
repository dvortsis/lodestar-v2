import { launch, context, settle, wake } from './lib.mjs';
const browser = await launch();
const ctx = await context(browser, { width: 390, height: 800, coarse: true });
const page = await ctx.newPage();
for (const p of ['/search?q=lantern', '/insights', '/settings/filters', '/alerts', '/settings/security', '/logs', '/discover', '/settings/sources', '/settings/about', '/compare?h=458dd874810e481cc1691c8e741ee25b49af0079']) {
  await page.goto(p); await settle(page); await wake(page); await settle(page, 300);
  const r = await page.evaluate(() => {
    const out = {}; let n = 0;
    for (const el of document.querySelectorAll('a[href],button,input:not([type=hidden]),select,summary,[role=button],[role=checkbox],[role=switch],[role=tab],[role=radio]')) {
      const r = el.getBoundingClientRect(); if (r.width < 2 || r.height < 2) continue; const cs = getComputedStyle(el); if (cs.visibility === 'hidden') continue;
      if (el.tagName === 'A' && cs.display === 'inline') continue; n++;
      // a label wrapping an input extends its target
      const lab = el.closest('label'); const rr = lab ? lab.getBoundingClientRect() : r;
      if (rr.width < 44 || rr.height < 44) { const k = `${el.tagName.toLowerCase()}${el.getAttribute('role') ? '[' + el.getAttribute('role') + ']' : ''}.${(el.className || '').toString().split(/\s+/)[0]} ${Math.round(rr.width)}x${Math.round(rr.height)} "${(el.getAttribute('aria-label') || el.textContent || el.type || '').trim().slice(0, 18)}"`; out[k] = (out[k] || 0) + 1; }
    }
    return { n, under: Object.entries(out).sort((a, b) => b[1] - a[1]).slice(0, 9), total: Object.values(out).reduce((a, b) => a + b, 0) };
  });
  console.log(p, 'interactive', r.n, 'under44', r.total, JSON.stringify(r.under));
}
await browser.close();
