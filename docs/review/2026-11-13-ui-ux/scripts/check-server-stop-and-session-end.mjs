// Failure paths: the server stops mid-session; the session ends with an unsaved Settings change.
import { execSync, spawn } from 'node:child_process';
import { launch, context, watch, settle, wake, shot, BASE, PASSWORD } from './lib.mjs';
const BIN = process.env.LODESTAR_BIN ?? new URL('../../../../target/release/lodestar', import.meta.url).pathname;
const FIX = process.env.FIXDIR ?? '/tmp/lodestar-fixture';
const start = () => { const c = spawn(BIN, ['serve', '--fixture', FIX], { env: { ...process.env, LODESTAR_HTTP_BIND: '127.0.0.1:8291', LODESTAR_FIXTURE_PASSWORD: PASSWORD }, detached: true, stdio: 'ignore' }); c.unref(); };
const healthy = async () => { for (let i = 0; i < 60; i++) { try { const r = await fetch(BASE + '/healthz'); if (r.ok) return true; } catch {} await new Promise(r => setTimeout(r, 1000)); } return false; };
const browser = await launch();
const say = async (page, label) => { const t = await page.evaluate(() => ({ pill: document.querySelector('[data-testid=status-pill]')?.innerText.replace(/\s+/g, ' ').trim(), h: [...document.querySelectorAll('main h1, main h2, main [role=heading]')].filter(e => e.getBoundingClientRect().width > 1).map(e => e.textContent.trim()).slice(0, 4), alerts: [...document.querySelectorAll('[role=alert], [role=status], .toast')].map(e => e.innerText.replace(/\s+/g, ' ').trim()).filter(Boolean).slice(0, 4), buttons: [...document.querySelectorAll('main button')].filter(e => e.getBoundingClientRect().width > 1).map(e => e.textContent.trim()).slice(0, 6), rows: document.querySelectorAll('[role=row]').length })); console.log(label, JSON.stringify(t).slice(0, 600)); await shot(page, 'f10-' + label, false); };
{ // A: server stops
  const ctx = await context(browser, { width: 1366, height: 900 }); const page = await ctx.newPage(); const log = watch(page);
  await page.goto('/search?q=lantern'); await settle(page); await wake(page); await say(page, 'a0-before');
  try { execSync('pkill -x lodestar'); } catch {} await page.waitForTimeout(4000); await say(page, 'a1-down-4s');
  await page.waitForTimeout(12000); await say(page, 'a2-down-16s');
  await page.getByTestId('query-editor').click({ timeout: 3000 }).then(async () => { await page.keyboard.type(' keeper'); await page.waitForTimeout(2500); }, (e) => console.log('  cannot type:', String(e).split('\n')[0].slice(0, 100))); await say(page, 'a3-typed-while-down');
  await page.getByTestId('nav-settings').click({ timeout: 3000 }).catch((e) => console.log('  nav click:', String(e).split('\n')[0].slice(0, 100))); await page.waitForTimeout(3000); await say(page, 'a4-nav-settings-while-down');
  start(); console.log('  healthy again:', await healthy()); await page.waitForTimeout(8000); await say(page, 'a5-back-8s');
  await page.waitForTimeout(25000); await say(page, 'a6-back-33s'); console.log('  url', page.url().replace(BASE, ''), 'pageErrors', log.pageErrors.slice(0, 2));
  await ctx.close(); }
{ // B: session ends mid-edit
  const ctx = await context(browser, { width: 1366, height: 900 }); const page = await ctx.newPage();
  await page.goto('/settings/crawler'); await settle(page); await wake(page);
  const target = (await page.getByRole('radio', { name: 'Aggressive' }).isChecked()) ? 'Balanced' : 'Aggressive';
  await page.getByRole('radio', { name: target }).check(); await settle(page, 500); await say(page, 'b0-dirty');
  await ctx.clearCookies(); await page.getByRole('button', { name: 'Save changes' }).click(); await page.waitForTimeout(2500); await say(page, 'b1-after-save-no-session'); console.log('  url', page.url().replace(BASE, ''));
  const pw = page.locator('input[type=password]'); if (await pw.count()) { await pw.first().fill(PASSWORD); await page.keyboard.press('Enter'); await page.waitForTimeout(3000); await settle(page, 500); await say(page, 'b2-after-sign-in'); console.log('  url', page.url().replace(BASE, ''), '| target still chosen:', await page.getByRole('radio', { name: target }).isChecked().catch(() => 'n/a'), '| unsaved bar:', await page.getByText(/unsaved change/).count()); }
  await ctx.close(); }
await browser.close();
