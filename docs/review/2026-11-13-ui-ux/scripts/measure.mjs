import { launch, context, settle, wake, save } from './lib.mjs';
const PAGES = [['search', '/search?q=lantern'], ['detail', '/t/458dd874810e481cc1691c8e741ee25b49af0079'], ['compare', '/compare?h=458dd874810e481cc1691c8e741ee25b49af0079,340d4ea26fa4cf61f0ee67685bd76dbd83853833'], ['discover', '/discover'], ['alerts', '/alerts'], ['alert1', '/alerts/1'], ['insights', '/insights'], ['ins-fetch', '/insights/fetch'], ['settings', '/settings'], ['s-crawler', '/settings/crawler'], ['s-filters', '/settings/filters'], ['s-security', '/settings/security'], ['s-sources', '/settings/sources'], ['s-appearance', '/settings/appearance'], ['s-outputs', '/settings/outputs'], ['s-logging', '/settings/logging'], ['logs', '/logs']];
const RAMP = new Set([0, 2, 4, 6, 8, 10, 12, 16, 20, 24, 28, 32, 36, 40, 48, 52, 56]);
const fn = (ramp) => {
  const RAMP = new Set(ramp);
  const vis = (el) => { const r = el.getBoundingClientRect(); const cs = getComputedStyle(el); return r.width > 1 && r.height > 1 && cs.visibility !== 'hidden' && cs.display !== 'none'; };
  const sel = (e) => { let s = e.tagName.toLowerCase(); const t = e.getAttribute('data-testid'); if (t) s += `[${t}]`; else if (typeof e.className === 'string' && e.className) s += '.' + e.className.split(/\s+/).slice(0, 3).join('.'); return s.slice(0, 70); };
  const add = (m, k, e) => { (m[k] ??= { n: 0, ex: [] }).n++; if (m[k].ex.length < 3) { const s = sel(e); if (!m[k].ex.includes(s)) m[k].ex.push(s); } };
  const out = { spacing: {}, radii: {}, heights: {}, fontFam: {}, type: {}, trans: {}, anim: {}, shadows: {}, gradients: [], icons: {}, borderW: {}, pills: [], nontab: [], prose: [], unlabeled: [], nested: 0, primaries: [], lefts: {}, tracking: {} };
  const main = document.querySelector('main') || document.body;
  for (const el of document.querySelectorAll('body *')) {
    if (!vis(el) || el.closest('svg') && el.tagName !== 'svg') continue;
    const cs = getComputedStyle(el);
    for (const p of ['paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft', 'marginTop', 'marginBottom', 'marginLeft', 'marginRight', 'rowGap', 'columnGap']) {
      const v = parseFloat(cs[p]); if (!isNaN(v) && v > 0 && !RAMP.has(Math.round(v * 100) / 100) && v < 200) add(out.spacing, `${p.replace(/Top|Right|Bottom|Left/, '')}:${Math.round(v * 100) / 100}`, el);
    }
    const br = cs.borderTopLeftRadius; const hasBox = cs.backgroundColor !== 'rgba(0, 0, 0, 0)' || parseFloat(cs.borderTopWidth) > 0;
    if (br !== '0px' && hasBox) { add(out.radii, br, el); const r = el.getBoundingClientRect(); if (parseFloat(br) >= Math.min(r.width, r.height) / 2 - 0.5 && r.height >= 24 && r.width > r.height * 1.5) out.pills.length < 8 && out.pills.push(sel(el) + ` ${Math.round(r.width)}x${Math.round(r.height)} r=${br}`); }
    if (parseFloat(cs.borderTopWidth) > 0 && cs.borderTopStyle !== 'none') add(out.borderW, cs.borderTopWidth + ' ' + cs.borderTopStyle, el);
    if (cs.backgroundImage.includes('gradient')) out.gradients.length < 6 && out.gradients.push(sel(el) + ' ' + cs.backgroundImage.slice(0, 60));
    if (cs.boxShadow !== 'none') add(out.shadows, cs.boxShadow.slice(0, 80), el);
    if (cs.transitionDuration && cs.transitionDuration !== '0s' && cs.transitionProperty !== 'none') add(out.trans, `${cs.transitionDuration} ${cs.transitionTimingFunction.slice(0, 32)} [${cs.transitionProperty.slice(0, 40)}]`, el);
    if (cs.animationName !== 'none') add(out.anim, `${cs.animationName} ${cs.animationDuration} ${cs.animationIterationCount}`, el);
    if (el.tagName === 'svg') { const r = el.getBoundingClientRect(); if (r.width <= 40) add(out.icons, `${Math.round(r.width)}x${Math.round(r.height)}`, el); }
    if (/^(BUTTON|INPUT|SELECT|TEXTAREA)$/.test(el.tagName) || el.classList.contains('btn')) { if (el.type !== 'checkbox' && el.type !== 'radio' && el.type !== 'range' && el.tagName !== 'TEXTAREA') { const r = el.getBoundingClientRect(); add(out.heights, `${el.tagName.toLowerCase()}${el.classList.contains('btn') ? '.btn' : ''}:${Math.round(r.height)}`, el); } }
    if (/^(INPUT|SELECT|TEXTAREA)$/.test(el.tagName) && el.type !== 'hidden') { const lab = (el.labels && el.labels.length) || el.getAttribute('aria-labelledby'); const lt = lab ? (el.labels?.[0]?.textContent || 'x').trim() : ''; if (!lt) out.unlabeled.length < 10 && out.unlabeled.push(sel(el) + ` aria-label="${el.getAttribute('aria-label') || ''}" ph="${el.getAttribute('placeholder') || ''}"`); }
    if (el.classList.contains('btn-primary')) out.primaries.push((el.textContent || '').trim().slice(0, 24));
    if (hasBox && parseFloat(br) >= 12 && parseFloat(cs.borderTopWidth) > 0) { const p = el.parentElement?.closest('*'); for (let a = el.parentElement; a && a !== document.body; a = a.parentElement) { const c = getComputedStyle(a); if (parseFloat(c.borderTopLeftRadius) >= 12 && parseFloat(c.borderTopWidth) > 0) { out.nested++; break; } } }
  }
  // text
  const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT); const seen = new Set();
  while (w.nextNode()) { const t = w.currentNode.nodeValue.trim(); const el = w.currentNode.parentElement; if (!t || !el || seen.has(el) || !vis(el)) continue; seen.add(el); const cs = getComputedStyle(el);
    add(out.fontFam, cs.fontFamily.split(',')[0].replace(/"/g, ''), el);
    const fs = parseFloat(cs.fontSize), lh = parseFloat(cs.lineHeight); add(out.type, `${fs}/${lh}/${cs.fontWeight}`, el);
    if (fs >= 20) add(out.tracking, `${fs}px ls=${cs.letterSpacing}`, el);
    if (/\d{2,}/.test(t) && !/^[\d\s]*[a-zA-Z]{6,}/.test(t) && cs.fontVariantNumeric.indexOf('tabular') < 0 && !cs.fontFamily.includes('Mono') && main.contains(el)) out.nontab.length < 14 && out.nontab.push(`${sel(el)} "${t.slice(0, 26)}"`);
    if (t.length > 140 && el.tagName === 'P') { const r = el.getBoundingClientRect(); const lines = Math.max(1, Math.round(r.height / lh)); const cpl = Math.round(el.textContent.trim().length / lines); if (cpl > 80) out.prose.length < 8 && out.prose.push(`${sel(el)} ${cpl} ch/line, ${Math.round(r.width)}px wide, ${fs}px`); }
  }
  // left edges of headings and section starts inside main
  for (const el of main.querySelectorAll('h1,h2,h3,[role=heading],section,form,table,p,ul')) { if (!vis(el)) continue; const x = Math.round(el.getBoundingClientRect().left); (out.lefts[x] ??= 0); out.lefts[x]++; }
  const h1 = main.querySelector('h1,[role=heading][aria-level="1"]'); out.h1 = h1 ? { x: Math.round(h1.getBoundingClientRect().left), y: Math.round(h1.getBoundingClientRect().top), fs: getComputedStyle(h1).fontSize, text: h1.textContent.trim().slice(0, 30), sr: h1.getBoundingClientRect().width <= 1 } : null;
  const first = [...main.querySelectorAll('*')].find((e) => vis(e) && e.children.length === 0 && e.textContent.trim()); out.firstTextX = first ? Math.round(first.getBoundingClientRect().left) : null;
  out.mainW = Math.round(main.getBoundingClientRect().width);
  return out;
};
const browser = await launch();
const res = {};
for (const [w, coarse] of [[1366, false], [390, true]]) {
  const ctx = await context(browser, { width: w, height: 900, coarse });
  const page = await ctx.newPage();
  for (const [n, p] of PAGES) { await page.goto(p); await settle(page); await wake(page); await settle(page, 300); res[`${n}@${w}`] = await page.evaluate(fn, [...RAMP]); process.stdout.write('.'); }
  await ctx.close();
}
// reduced motion
{ const ctx = await context(browser, { width: 1366, height: 900, reducedMotion: 'reduce' }); const page = await ctx.newPage(); await page.goto('/search?q=lantern'); await settle(page); await wake(page);
  res.reduced = await page.evaluate(() => { const cs = getComputedStyle(document.documentElement); const o = {}; for (const v of ['--duration-fast', '--duration-drawer-in', '--duration-banner', '--duration-reduced-fade']) o[v] = cs.getPropertyValue(v); const t = {}; for (const el of document.querySelectorAll('body *')) { const c = getComputedStyle(el); if (c.transitionDuration !== '0s' && c.transitionProperty !== 'none') t[c.transitionDuration + ' ' + c.transitionProperty.slice(0, 30)] = (t[c.transitionDuration + ' ' + c.transitionProperty.slice(0, 30)] || 0) + 1; if (c.animationName !== 'none') t['anim ' + c.animationName + ' ' + c.animationDuration] = 1; } o.live = t; return o; }); await ctx.close(); }
save('_measure', res); await browser.close(); console.log('done');
