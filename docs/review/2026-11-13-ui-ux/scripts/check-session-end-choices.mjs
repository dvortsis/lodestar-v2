import { launch, context, settle, wake, shot, BASE, PASSWORD } from './lib.mjs';
const browser = await launch();
for (const choice of ['Save changes', 'Discard']) {
  const ctx = await context(browser, { width: 1366, height: 900 }); const page = await ctx.newPage();
  await page.goto('/settings/crawler'); await settle(page); await wake(page);
  const target = (await page.getByRole('radio', { name: 'Aggressive' }).isChecked()) ? 'Balanced' : 'Aggressive';
  await page.getByRole('radio', { name: target }).check(); await settle(page, 400);
  await ctx.clearCookies(); await page.getByRole('button', { name: 'Save changes' }).click(); await page.waitForTimeout(2000);
  const dlg = page.getByRole('dialog'); console.log(choice, '| dialog:', (await dlg.count()) ? (await dlg.innerText()).replace(/\s+/g, ' ').slice(0, 120) : 'none');
  await dlg.getByRole('button', { name: choice }).click(); await page.waitForTimeout(3000);
  console.log('  after', choice, '->', page.url().replace(BASE, ''), '| dialog still:', await page.getByRole('dialog').count(), '| text:', (await page.locator('main').innerText()).replace(/\s+/g, ' ').slice(0, 160));
  await shot(page, 'f11-' + choice.replace(' ', '-'), false);
  const pw = page.locator('input[type=password]');
  if (await pw.count()) { await pw.first().fill(PASSWORD); await page.keyboard.press('Enter'); await page.waitForTimeout(3500); await settle(page, 400);
    console.log('  after sign-in ->', page.url().replace(BASE, ''), '|', target, 'checked:', await page.getByRole('radio', { name: target }).isChecked().catch(() => 'n/a'), '| unsaved bar:', await page.getByText(/unsaved change/).count(), '| banners:', JSON.stringify(await page.evaluate(() => [...document.querySelectorAll('[role=status],[role=alert]')].map(e => e.innerText.replace(/\s+/g, ' ').trim()).filter(Boolean).slice(0, 3))));
    await shot(page, 'f11-' + choice.replace(' ', '-') + '-after-signin', false); }
  await ctx.close();
}
await browser.close();
