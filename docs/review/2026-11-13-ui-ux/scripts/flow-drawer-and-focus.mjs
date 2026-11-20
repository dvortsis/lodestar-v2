import { launch, context, watch, settle, wake, shot } from './lib.mjs';
const browser = await launch();
// mobile drawer scroll check
{
const ctx = await context(browser, { width: 390, height: 800, coarse: true });
const page = await ctx.newPage();
await page.goto('/search?q=lantern'); await settle(page); await wake(page);
await page.locator('[role=row]').nth(0).tap(); await settle(page, 1000);
const info = await page.evaluate(() => {
  const out = [];
  for (const el of document.querySelectorAll('body *')) { const cs = getComputedStyle(el); if (/(auto|scroll)/.test(cs.overflowY) && el.scrollHeight > el.clientHeight + 5) out.push([el.tagName, el.id, (el.className||'').toString().slice(0,60), el.scrollHeight, el.clientHeight, cs.position]); }
  return { scrollers: out, docH: document.documentElement.scrollHeight, bodyOverflow: getComputedStyle(document.body).overflow, htmlOverflow: getComputedStyle(document.documentElement).overflow, h2: [...document.querySelectorAll('#search-drawer h2, #search-drawer h3, [role=dialog] h2, [role=dialog] h3')].map(h => h.textContent.trim()) };
});
console.log(JSON.stringify(info));
// touch scroll gesture via CDP
const cdp = await ctx.newCDPSession(page);
await cdp.send('Input.synthesizeScrollGesture', { x: 200, y: 600, yDistance: -900, speed: 2000 });
await settle(page, 500); await shot(page, 'f3-01-mobile-detail-scrolled', false);
await cdp.send('Input.synthesizeScrollGesture', { x: 200, y: 600, yDistance: -1500, speed: 3000 });
await settle(page, 500); await shot(page, 'f3-02-mobile-detail-bottom', false);
console.log('headings', await page.evaluate(() => [...document.querySelectorAll('h1,h2,h3')].filter(h=>h.getBoundingClientRect().width>0).map(h => h.tagName+':'+h.textContent.trim().slice(0,30))));
await ctx.close();
}
// desktop full detail page sections
{
const ctx = await context(browser, { width: 1366, height: 900 });
const page = await ctx.newPage();
const { readFileSync } = await import('node:fs');
const man = JSON.parse(readFileSync((process.env.FIXDIR ?? '/tmp/lodestar-fixture') + '/fixture.json', 'utf8'));
const withSwarm = man.torrents.find((t) => t.swarm && t.swarm.seeders > 100 && t.name.includes('Lantern'));
await page.goto('/t/' + withSwarm.hash); await settle(page); await wake(page);
for (let i = 0; i < 8; i++) { await page.mouse.wheel(0, 900); await page.waitForTimeout(350); }
await settle(page, 800);
console.log('desktop headings', await page.evaluate(() => [...document.querySelectorAll('h1,h2,h3,h4')].map(h => h.tagName+':'+h.textContent.trim().slice(0,40))));
await page.evaluate(() => scrollTo(0, 0)); await settle(page, 300);
await shot(page, 'f3-03-detail-full', true);
// browse: scroll far, open drawer
await page.goto('/search?browse=1'); await settle(page); await wake(page);
for (let i = 0; i < 6; i++) { await page.mouse.wheel(0, 1200); await page.waitForTimeout(300); }
await settle(page, 500);
const rows = page.locator('[role=row]'); const n = await rows.count();
await rows.nth(Math.max(0, n - 6)).click(); await settle(page, 1000);
await shot(page, 'f3-04-drawer-deep', false);
console.log('scrollY', await page.evaluate(() => scrollY), 'drawer rect', await page.evaluate(() => { const d = document.querySelector('#search-drawer'); const r = d.getBoundingClientRect(); const c = d.firstElementChild?.getBoundingClientRect(); return [r.top, r.height, c?.top, c?.height, getComputedStyle(d.firstElementChild || d).position]; }));
await page.keyboard.press('j'); await settle(page, 600); await shot(page, 'f3-05-drawer-j', false);
// focus ring visibility: tab through
await page.goto('/search?q=lantern'); await settle(page); await wake(page);
for (let i = 0; i < 9; i++) await page.keyboard.press('Tab');
await shot(page, 'f3-06-tab9', false);
console.log('focus@9', await page.evaluate(() => { const a = document.activeElement; const cs = getComputedStyle(a); return [a.tagName, a.getAttribute('data-testid'), (a.textContent||'').trim().slice(0,30), cs.outlineStyle, cs.outlineWidth, cs.outlineColor]; }));
const order = [];
await page.goto('/search?q=lantern'); await settle(page); await wake(page);
for (let i = 0; i < 40; i++) { await page.keyboard.press('Tab'); order.push(await page.evaluate(() => { const a = document.activeElement; const cs = getComputedStyle(a); return (a.getAttribute('aria-label') || a.textContent || a.tagName).trim().slice(0, 22) + (cs.outlineStyle === 'none' && !cs.boxShadow.includes('rgb') ? ' [NO RING]' : ''); })); }
console.log('tab order', JSON.stringify(order));
await ctx.close();
}
await browser.close();
