// Settings → Language & region (S-settings-2) against the OpenAPI mock: renders only the region
// fields, saves an edit by keyboard, and passes axe.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];

const f = (name: string) => ({
	name,
	env: null,
	label_key: `settings.preferences.${name}.label`,
	help_key: `settings.preferences.${name}.help`,
	read_only: false,
	restart_required: false
});
const index = {
	sections: [
		{
			section: 'preferences',
			label_key: 'settings.preferences.label',
			help_key: 'settings.preferences.help',
			fields: ['theme', 'language', 'time_zone', 'first_day_of_week'].map(f),
			schema: {
				type: 'object',
				properties: {
					theme: { type: 'string', 'x-group': 'appearance', 'x-order': 0 },
					language: { type: 'string', 'x-group': 'region', 'x-order': 8, 'x-widget': 'language' },
					time_zone: { type: 'string', 'x-group': 'region', 'x-order': 9, 'x-widget': 'time-zone' },
					first_day_of_week: {
						type: 'string',
						enum: ['auto', 'monday', 'saturday', 'sunday'],
						'x-group': 'region',
						'x-order': 10
					}
				}
			}
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
const values: Values[] = [
	{
		section: 'preferences',
		revision: 4,
		restart_required: [],
		fields: {
			theme: v('riverstone'),
			language: v(''),
			time_zone: v(''),
			first_day_of_week: v('auto')
		}
	}
];

test('Language & region saves a language at once, by keyboard, and applies it', async ({
	page,
	go,
	mock,
	kbd
}) => {
	const backend = new SettingsBackend(index, values).attach(mock as unknown as MockLike);
	await go('/settings/language');
	await expect(page.getByRole('heading', { level: 1, name: 'Language & region' })).toBeVisible();
	const language = page.getByLabel('Language', { exact: true });
	await expect(language).toBeVisible();
	await expect(page.getByLabel('Theme')).toHaveCount(0);
	await expectNoA11yViolations(page);
	await kbd.tabTo(language);
	await language.selectOption('zh');
	await expect.poll(() => backend.patches.length).toBe(1);
	expect(backend.patches).toEqual([{ preferences: { if_revision: 4, language: 'zh' } }]);
	await expect(page.locator('html')).toHaveAttribute('lang', 'zh');
});
