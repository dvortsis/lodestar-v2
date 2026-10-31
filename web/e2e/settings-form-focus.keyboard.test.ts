// Keyboard focus and local UI state survive a save and the resync after it (X-form-remount,
// WCAG 2.4.3): the control the user used stays focused, and the Logging list keeps its search.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });

test.describe('keyboard: focus stays through a resync', () => {
	test('a generated screen keeps focus on the radio after saving', async ({
		page,
		go,
		kbd,
		mock
	}) => {
		const backend = new SettingsBackend().attach(mock);
		await go('/dev/settings-form');
		const group = page.locator('[data-field="crawler.intensity"]').getByRole('radiogroup');
		await expect(group).toBeVisible();
		await kbd.tabTo(page.getByRole('radio', { name: 'balanced' }));
		await kbd.press('ArrowDown');
		const chosen = page.getByRole('radio', { name: 'aggressive' });
		await expect(chosen).toBeChecked();
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		// The server announces the change; the form refetches the section.
		backend.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
		await expect.poll(() => backend.patches.length).toBe(1);
		await expect(chosen).toBeFocused();
		await expect(page.getByTestId('save-bar')).toBeVisible();
		await expectNoA11yViolations(page);
	});

	test('the Logging event list keeps focus, search text and open groups', async ({
		page,
		go,
		kbd,
		mock
	}) => {
		type Index = components['schemas']['SettingsIndex'];
		type Values = components['schemas']['SectionValues'];
		const props = {
			events: { type: 'object', 'x-widget': 'log-events', 'x-group': 'events', 'x-order': 1 }
		};
		const index = {
			sections: [
				{
					section: 'logging',
					label_key: 'settings.logging.label',
					help_key: 'settings.logging.help',
					fields: [
						{
							name: 'events',
							env: null,
							label_key: 'settings.logging.events.label',
							help_key: 'settings.logging.events.help',
							read_only: false,
							restart_required: false
						}
					],
					schema: { type: 'object', properties: props }
				}
			]
		} as unknown as Index;
		const field = {
			env: null,
			read_only: false,
			restart_required: false,
			source: 'default' as const
		};
		const values: Values[] = [
			{
				section: 'logging',
				revision: 1,
				restart_required: [],
				fields: { events: { value: {}, ...field } }
			}
		];
		const ev = (id: string, component: string) => ({
			id,
			component,
			level: 'info',
			default_on: true,
			current: true,
			required: false,
			opt_in: null,
			kinds: [],
			label_key: `logs.event.${id}.label`,
			help_key: `logs.event.${id}.help`,
			rate_per_hour: 10
		});
		const backend = new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getLogCatalog', () => ({
			status: 200,
			body: {
				now: Math.floor(Date.now() / 1000),
				events: [ev('pipeline.stored', 'pipeline'), ev('pipeline.dropped', 'pipeline')],
				kinds: [],
				outputs: [],
				presets: { data: 'private', verbosity: 'normal', temporary: null }
			}
		}));
		on('getLogFiles', () => ({
			status: 200,
			body: { files: [], folder: '/data/logs', index_readable: true, reason: null, state: 'on' }
		}));
		await go('/settings/logging');
		const search = page.getByRole('searchbox', { name: 'Search events' });
		await kbd.tabTo(search, 60);
		await page.keyboard.type('stored');
		const toggle = page.getByRole('switch', { name: 'pipeline.stored' });
		await kbd.tabTo(toggle, 20);
		await kbd.press('Space');
		await expect(toggle).not.toBeChecked();
		await kbd.press('Control+s');
		await expect(page.getByTestId('save-bar')).toContainText('Saved');
		backend.change('logging', { events: { value: { 'pipeline.stored': false }, source: 'db' } });
		await expect.poll(() => backend.patches.length).toBe(1);
		await expect(toggle).toBeFocused();
		await expect(search).toHaveValue('stored');
		await expectNoA11yViolations(page);
	});
});
