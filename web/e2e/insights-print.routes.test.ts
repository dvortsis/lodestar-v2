// The Insights print layout (§10.9.6) under print media, and the digest preview on Settings →
// Insights & digest (§10.9.7) by keyboard alone, with axe.
import { crawlerFixture, overviewFixture } from '../src/lib/features/insights/overview/fixtures';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

test('print media: the chrome is gone and the footer shows the URL and time', async ({
	page,
	mock,
	go
}) => {
	mock.on('getInsightsOverview', overviewFixture());
	mock.on('getCrawler', crawlerFixture());
	await go('/insights');
	await expect(page.getByTestId('insights-kpis')).toBeVisible();
	await expect(page.getByTestId('insights-print-footer')).toBeAttached();
	await page.emulateMedia({ media: 'print' });
	await expect(page.getByTestId('insights-print-footer')).toBeVisible();
	await expect(page.getByTestId('insights-print-footer')).toContainText('/insights');
	await expect(page.getByTestId('insights-print-footer')).toContainText('Generated');
	await expect(page.getByTestId('insights-print-head')).toBeVisible();
	await expect(page.locator('header').first()).toBeHidden();
	await expect(page.getByTestId('insights-bar')).toBeHidden();
	await expect(page.getByTestId('insights-strip')).toBeHidden();
	await expect(page.locator('#insights-detail')).toBeHidden();
	await expect(page.getByTestId('insights-kpis')).toBeVisible();
	await expect(page.locator('#insights-exceptions')).toBeVisible();
	await expect(page.locator('#insights-funnel')).toBeVisible();
	await expect(page.locator('#insights-movement')).toBeVisible();
	for (const id of ['status', 'exceptions', 'movement']) {
		await expect(page.locator(`#insights-${id} .ins-sub`)).toBeVisible();
	}
	await expect(page.getByRole('button').filter({ visible: true })).toHaveCount(0);
	await expect(page.locator('html')).toHaveAttribute('data-theme', 'riverstone-light');
	await page.emulateMedia({ media: 'screen' });
	await expect(page.getByTestId('insights-print-footer')).toBeHidden();
});

for (const width of [800, 1024, 1920]) {
	test(`print: one page on A4 and Letter at ${width} px`, async ({ page, mock, go }) => {
		await page.setViewportSize({ width, height: 900 });
		mock.on('getInsightsOverview', overviewFixture());
		mock.on('getCrawler', crawlerFixture());
		await go('/insights');
		await expect(page.getByTestId('insights-print-footer')).toBeAttached();
		await expect(page.getByTestId('insights-line-new')).toBeVisible();
		for (const format of ['A4', 'Letter'] as const) {
			const pdf = await page.pdf({ format });
			const pages = (pdf.toString('latin1').match(/\/Type\s*\/Page[^s]/g) ?? []).length;
			expect(pages, format).toBe(1);
		}
	});
}

type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];
const properties = {
	digest: { type: 'string', enum: ['off', 'daily', 'weekly'], 'x-group': 'digest', 'x-order': 0 },
	digest_channels: { type: 'array', 'x-group': 'digest', 'x-order': 3 }
};
const index = {
	sections: [
		{
			section: 'insights',
			label_key: 'settings.insights.label',
			help_key: 'settings.insights.help',
			fields: Object.keys(properties).map((name) => ({
				name,
				env: null,
				label_key: `settings.insights.${name}.label`,
				help_key: `settings.insights.${name}.help`,
				read_only: false,
				restart_required: false
			})),
			schema: { type: 'object', properties }
		}
	]
} as unknown as Index;
const v = (value: unknown) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const
});
const values = (digest: string): Values[] => [
	{
		section: 'insights',
		revision: 1,
		restart_required: [],
		fields: { digest: v(digest), digest_channels: v([]) }
	}
];

test('digest preview: keyboard-only, text only, no violations', async ({ page, mock, go, kbd }) => {
	new SettingsBackend(index, values('weekly')).attach(mock as unknown as MockLike);
	mock.on('getDigestPreview', {
		generated_at: 1800000000,
		html: '<!DOCTYPE html><html><body><b>hi</b></body></html>',
		period: 'weekly',
		range: { bucket: 'hour', from: 1799395200, to: 1800000000, tz: 'UTC' },
		subject: 'Lodestar weekly digest',
		text: 'Found <b>12</b> new torrents\n'
	});
	await go('/settings/insights');
	const button = page.getByRole('button', { name: 'Preview digest' });
	await kbd.tabTo(button);
	await kbd.press('Enter');
	const text = page.getByTestId('digest-preview-text');
	await expect(text).toHaveText('Found <b>12</b> new torrents');
	await expect(text.locator('b')).toHaveCount(0);
	await expect(page.getByTestId('digest-preview-subject')).toContainText('Lodestar weekly digest');
	await expectNoA11yViolations(page);
	await kbd.tabTo(text);
	await expect(text).toBeFocused();
});

test('digest preview: off says how to turn it on', async ({ page, mock, go, kbd }) => {
	new SettingsBackend(index, values('off')).attach(mock as unknown as MockLike);
	await go('/settings/insights');
	await kbd.tabTo(page.getByRole('button', { name: 'Preview digest' }));
	await kbd.press('Enter');
	await expect(page.getByTestId('digest-preview-off')).toContainText('Choose Daily or Weekly');
	await expectNoA11yViolations(page);
});
