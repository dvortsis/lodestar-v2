import { launch, context, watch, settle, shot, axe, probe } from './lib.mjs';
const TOKEN = process.env.TOKEN;
const browser = await launch();
const ctx = await browser.newContext({ baseURL: 'http://127.0.0.1:8292', viewport: { width: 1366, height: 900 }, deviceScaleFactor: 1 });
const page = await ctx.newPage(); const log = watch(page);
let i = 0;
const snap = async (label) => { await settle(page, 500); const n = `f5-${String(++i).padStart(2, '0')}-${label}`; await shot(page, n, true); const a = await axe(page).catch(() => []); const p = await probe(page); console.log(n, page.url().replace('http://127.0.0.1:8292', ''), '| h:', JSON.stringify(p.headings.slice(0, 4)), '| axe:', JSON.stringify(a.map(v => v.id + ':' + v.n)), '| buttons:', JSON.stringify(await page.evaluate(() => [...document.querySelectorAll('main button, main a.btn')].filter(b => b.getBoundingClientRect().width > 0).map(b => (b.textContent || b.getAttribute('aria-label') || '').trim().slice(0, 28) + (b.disabled ? '(off)' : '')).slice(0, 14)))); };
await page.goto('/'); await snap('root');
await page.goto('/setup'); await snap('setup');
await page.goto('/'); await settle(page, 800); await page.mouse.move(200,300); await page.mouse.click(200,300); await settle(page, 2500); console.log('root after click ->', page.url()); await snap('root-after-click');
await page.goto('/setup'); await settle(page, 600);
const describe = async () => console.log('  inputs', JSON.stringify(await page.evaluate(() => [...document.querySelectorAll('main input, main select, main textarea')].filter(i=>i.getBoundingClientRect().width>0).map(i => i.type + ':' + (i.labels?.[0]?.textContent || i.getAttribute('aria-label') || '').trim().slice(0, 40)))));
await describe();
await page.getByRole('button', { name: 'Continue' }).click(); await snap('legal-unchecked-continue');
await page.locator('input[type=checkbox]').first().check(); await page.getByRole('button', { name: 'Continue' }).click(); await settle(page, 1000); await snap('token-step'); await describe();
const tok = page.getByLabel(/Setup token/);
await tok.fill('0000-0000-0000-0000'); await page.keyboard.press('Enter'); await settle(page, 1000); await snap('bad-token-next'); await describe();
let pw = page.locator('main input[type=password]');
for (let j = 0; j < await pw.count(); j++) await pw.nth(j).fill('short'); await page.keyboard.press('Enter'); await settle(page, 800); await snap('weak-pw');
for (let j = 0; j < await pw.count(); j++) await pw.nth(j).fill('correct horse battery'); await page.keyboard.press('Enter'); await settle(page, 1500); await snap('bad-token-submitted'); await describe();
if (await page.getByLabel(/Setup token/).count()) { await page.getByLabel(/Setup token/).fill(TOKEN); await page.keyboard.press('Enter'); await settle(page, 1000); await snap('good-token-next'); }
pw = page.locator('main input[type=password]');
for (let j = 0; j < await pw.count(); j++) await pw.nth(j).fill('correct horse battery'); await page.keyboard.press('Enter'); await settle(page, 2000); await snap('after-pw'); await describe();
// Walk the wizard: press the primary button repeatedly.
for (let k = 0; k < 16; k++) {
  const url = page.url();
  const primary = page.locator('main button.btn-primary:not([disabled]), main a.btn-primary').last();
  if (!(await primary.count())) { console.log('no primary at step', k); const cbs = page.locator('main input[type=checkbox]:not(:checked)'); const c = await cbs.count(); for (let j = 0; j < c; j++) await cbs.nth(j).check().catch(() => {}); if (!c) break; await snap('checked-' + k); continue; }
  const label = (await primary.textContent() || '').trim();
  await primary.click().catch((e) => console.log('click fail', String(e).slice(0, 100)));
  await settle(page, 1200);
  await snap('wiz-after-' + label.replace(/\W+/g, '_').slice(0, 20));
  if (/\/search/.test(page.url())) break;
}
console.log(JSON.stringify(log.http.slice(0, 10)), JSON.stringify(log.pageErrors.slice(0, 3)));
await browser.close();
