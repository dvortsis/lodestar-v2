import { launch, context, watch, settle, wake, shot, probe, axe } from './lib.mjs';
const browser = await launch();
const run = async (name, opts, fn) => { const ctx = await context(browser, opts); const page = await ctx.newPage(); const log = watch(page); try { await fn(page, ctx); console.log('ok', name); } catch (e) { console.log('FAIL', name, String(e).slice(0, 250)); } const errs = log.console.concat(log.pageErrors).filter(e => !/40[0-9]|503/.test(e)); if (errs.length) console.log('  errs', name, errs.slice(0,3)); await ctx.close(); };
for (const w of [1440, 1920]) await run('drawer' + w, { width: w, height: 1000 }, async (page) => { await page.goto('/search?q=lantern'); await settle(page); await wake(page); await page.locator('[role=row]').nth(1).click(); await settle(page, 1000); await shot(page, `f4-drawer-${w}`, false); });
await run('settings', { width: 1366, height: 900 }, async (page) => {
  await page.goto('/settings/crawler'); await settle(page); await wake(page);
  await page.getByRole('radio', { name: 'Aggressive' }).check(); await settle(page, 600); await shot(page, 'f4-s1-dirty', false);
  await page.locator('button[aria-haspopup="menu"], button[aria-label*="options" i], button[aria-label*="menu" i]').nth(2).click().catch(() => {}); await settle(page, 400); await shot(page, 'f4-s2-fieldmenu', false);
  await page.keyboard.press('Escape');
  await page.getByRole('link', { name: 'Network & VPN' }).click(); await settle(page, 600); await shot(page, 'f4-s3-leave', false);
  await page.keyboard.press('Escape');
  await page.goto('/settings'); await settle(page); await wake(page);
  await page.getByRole('searchbox').first().fill('port'); await settle(page, 500); await shot(page, 'f4-s4-search-port', false);
  await page.goto('/settings/appearance'); await settle(page); await wake(page);
  await page.getByRole('button', { name: 'Browse themes' }).click(); await settle(page, 1200); await shot(page, 'f4-s5-themes', false);
  await page.keyboard.press('Escape');
  await page.goto('/settings/filters'); await settle(page); await wake(page);
  await page.getByText('What each pack hides').click(); await settle(page, 400);
  await page.getByText('What each pack hides').scrollIntoViewIfNeeded(); await shot(page, 'f4-s6-packs-desc', false);
  await page.getByRole('button', { name: 'Turn off CS-2a' }).click(); await settle(page, 800); await shot(page, 'f4-s7-turnoff-cs', false);
  await page.keyboard.press('Escape');
  await page.goto('/settings/sources'); await settle(page); await wake(page); await shot(page, 'f4-s8-sources', false);
  await page.goto('/logs'); await settle(page); await wake(page); await shot(page, 'f4-s9-logs', false);
});
for (const [w, coarse] of [[1366, false], [390, true]]) await run('auth' + w, { width: w, height: 850, signedIn: false, coarse }, async (page) => {
  for (const p of ['login', 'setup', 'welcome']) { await page.goto('/' + p); await settle(page, 800); await shot(page, `f4-auth-${p}-${w}`, true); console.log('  ', p, page.url(), JSON.stringify((await axe(page)).map(v => v.id + ':' + v.n))); }
  await page.goto('/login'); await settle(page, 500); await page.locator('input[type=password]').first().fill('wrong password here'); await page.keyboard.press('Enter'); await settle(page, 1200); await shot(page, `f4-auth-login-wrong-${w}`, true);
  await page.goto('/search'); await settle(page, 1000); console.log('   signed-out /search ->', page.url());
});
await run('zh', { width: 1366, height: 900, locale: 'zh-CN' }, async (page) => {
  for (const [n, p] of [['search', '/search?q=lantern'], ['insights', '/insights'], ['crawler', '/settings/crawler'], ['filters', '/settings/filters'], ['alerts', '/alerts']]) { await page.goto(p); await settle(page); await wake(page); await settle(page, 400); await shot(page, `f4-zh-${n}`, false);
    const latin = await page.evaluate(() => { const out = new Set(); const w = document.createTreeWalker(document.querySelector('main'), NodeFilter.SHOW_TEXT); while (w.nextNode()) { const t = w.currentNode.nodeValue.trim(); const el = w.currentNode.parentElement; if (!t || !el || el.getBoundingClientRect().width === 0) continue; if (/[A-Za-z]{3,}/.test(t) && !/[一-鿿]/.test(t) && !el.closest('code,kbd,[class*=mono],.rr-raw,.rr-title,input')) out.add(t.slice(0, 40)); } return [...out].slice(0, 30); }); console.log('  zh latin', n, JSON.stringify(latin)); }
});
await run('dark', { width: 1366, height: 900, mode: 'dark' }, async (page) => {
  for (const [n, p] of [['search', '/search?q=lantern'], ['insights', '/insights'], ['crawler', '/settings/crawler']]) { await page.goto(p); await settle(page); await wake(page); await settle(page, 400); await shot(page, `f4-dark-${n}`, false); }
});
await browser.close();
