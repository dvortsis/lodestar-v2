import { launch, context, watch, settle, wake, shot } from './lib.mjs';
const browser = await launch();
const ctx = await context(browser, { width: 1366, height: 900 });
await ctx.grantPermissions(['clipboard-read', 'clipboard-write']);
const page = await ctx.newPage(); const log = watch(page);
const texts = async (sel) => page.evaluate((s) => [...document.querySelectorAll(s)].filter(e => e.getBoundingClientRect().width > 0).map(e => (e.innerText || e.getAttribute('aria-label') || '').trim().replace(/\s+/g, ' ').slice(0, 160)), sel);
const step = async (name, fn) => { try { const r = await fn(); await settle(page, 400); await shot(page, 'f7-' + name, false); console.log('ok', name, r ? JSON.stringify(r).slice(0, 700) : ''); } catch (e) { console.log('FAIL', name, String(e).split('\n')[0].slice(0, 200)); } };
await page.goto('/search'); await settle(page); await wake(page);
// A: plain-word search and grab
await step('a1-type', async () => { await page.getByTestId('query-editor').click(); await page.keyboard.type('lantern keeper 1080p'); await settle(page, 900); return { chips: await texts('[data-testid*=chip], [data-testid*=assist] button'), strip: await texts('[data-testid*=filter-strip] *:not(:has(*))') }; });
await page.keyboard.press('Escape');
await step('a2-copy', async () => { const row = page.locator('[role=row]').nth(1); await row.hover(); await row.getByRole('button', { name: /Copy magnet/ }).click(); await settle(page, 600); const clip = await page.evaluate(() => navigator.clipboard.readText()).catch(() => 'n/a'); return { toasts: await texts('[role=status], [role=alert], .toast *:not(:has(*))'), clip: clip.slice(0, 60) }; });
await step('a3-more-menu', async () => { const row = page.locator('[role=row]').nth(1); await row.hover(); await row.getByRole('button', { name: /More actions/ }).click(); await settle(page, 400); return await texts('[role=menuitem]'); });
await page.keyboard.press('Escape');
await step('a4-send-key', async () => { await page.locator('[role=row]').nth(1).focus(); await page.keyboard.press('s'); await settle(page, 600); return await texts('[role=status], [role=alert], [role=dialog] h2'); });
await page.keyboard.press('Escape');
// B: natural phrasing and builder
await step('b1-natural', async () => { await page.goto('/search'); await settle(page); await wake(page); await page.getByTestId('query-editor').click(); await page.keyboard.type('4k movies from 2020 over 10gb'); await settle(page, 700); const nat = page.getByRole('button', { name: /Natural phrasing/ }); await nat.click(); await settle(page, 900); return await texts('[data-testid*=natural] *:not(:has(*))'); });
await step('b2-builder', async () => { await page.goto('/search?q=lantern'); await settle(page); await wake(page); await page.getByTestId('query-editor').click(); await settle(page, 300); await page.getByRole('button', { name: 'Builder' }).click(); await settle(page, 700); return await texts('[data-testid*=builder] label, [data-testid*=builder] select, [data-testid*=builder] button'); });
// wrong-but-plausible syntax
for (const [n, q] of [['c1', 'seeders>5'], ['c2', 'size > 4gb'], ['c3', 'year:2020 type:film'], ['c4', '"lantern keeper" -cam']]) await step('c-' + n, async () => { await page.goto('/search?q=' + encodeURIComponent(q)); await settle(page); await wake(page); return { q, err: await texts('[data-testid*=query-error], [data-testid*=parse-error], .text-error'), count: await texts('[data-testid=results-count], [data-testid*=count]'), chips: await texts('[data-testid*=chip]') }; });
// D: custom date
await step('d1-custom-date', async () => { await page.goto('/search?q=lantern'); await settle(page); await wake(page); await page.getByRole('button', { name: 'Custom' }).click(); await settle(page, 500); return await texts('[data-testid*=custom] label, [role=dialog] label, [data-testid*=discovered] input'); });
// E: save search end to end
await step('e1-save', async () => { await page.goto('/search?q=lantern+res%3A1080p'); await settle(page); await wake(page); await page.getByRole('button', { name: 'Save search' }).click(); await settle(page, 500); await page.getByLabel(/Name/).fill('Lantern 1080p'); await page.getByRole('button', { name: 'Save search' }).last().click(); await settle(page, 900); return { url: page.url().replace(/^.*8291/, ''), toasts: await texts('[role=status], [role=alert]') }; });
// F: add download client
await step('f1-add-client', async () => { await page.goto('/settings/integrations'); await settle(page); await wake(page); await page.getByRole('button', { name: 'Add a download client' }).click(); await settle(page, 700); return await texts('main label, main select option, [role=dialog] label, [role=dialog] option'); });
await step('f2-test-empty', async () => { await page.getByRole('button', { name: /Test connection/ }).first().click(); await settle(page, 1500); return await texts('[role=status], [role=alert], .text-error, [data-testid*=test-result]'); });
// G: notifications add ntfy
await step('g1-ntfy', async () => { await page.goto('/settings/notifications'); await settle(page); await wake(page); await page.getByRole('button', { name: 'Add' }).first().click(); await settle(page, 500); return await texts('main input, [role=dialog] label'); });
// H: insights drill-through
await step('h1-loss-link', async () => { await page.goto('/insights'); await settle(page); await wake(page); await page.getByRole('link', { name: /No peers found/ }).first().click(); await settle(page, 900); return { url: page.url().replace(/^.*8291/, ''), chips: await texts('[data-testid*=chip], [data-testid*=reason]') }; });
await step('h2-bar-to-search', async () => { await page.goto('/insights'); await settle(page); await wake(page); await page.getByRole('link', { name: /^Video/ }).first().click(); await settle(page, 1200); return { url: decodeURIComponent(page.url().replace(/^.*8291/, '')), count: await texts('[data-testid*=results-count], [data-testid*=count]') }; });
await step('h3-intensity-help', async () => { await page.goto('/insights'); await settle(page); await wake(page); await page.getByText('Aggressive', { exact: true }).click(); await settle(page, 700); return await texts('[role=dialog] *:not(:has(*)), [role=status]'); });
console.log('errs', JSON.stringify(log.pageErrors.slice(0, 3)), JSON.stringify(log.http.slice(0, 8)));
await browser.close();
