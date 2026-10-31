// Settings → Logging's outputs on the real form engine (fakeServer): the Windows Event Log setting
// only on a Windows service, an Advanced field with an error shown rather than hidden, and the
// preview following the staged values, Verbosity left out while the environment sets it.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { initLocale } from '$lib/i18n/locale.svelte';
import { createSettingsForm, type SettingsForm } from '../form';
import { loadMessages } from '../form/messages';
import { fakeServer } from '../form/testing/fake-server';
import { spec, sv, values } from '../form/testing/values';
import Outputs from './Outputs.svelte';
import Preview from './Preview.svelte';

vi.setConfig({ testTimeout: 30_000 });
const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

// The outputs' labels are in the `logoutput` map, as LoggingScreen points them.
const out = (key: `logging.${string}`, over: Parameters<typeof spec>[1]) => {
	const name = key.split('.')[1];
	return spec(key, {
		labelKey: `settings.logoutput.${name}.label`,
		helpKey: `settings.logoutput.${name}.help`,
		...over
	});
};
const SPECS = [
	spec('logging.verbosity', {
		kind: 'enum',
		enum: ['normal', 'detailed'],
		group: 'events',
		preview: true
	}),
	out('logging.console_format', { kind: 'enum', enum: ['text', 'json'], group: 'console' }),
	out('logging.file_enabled', { kind: 'boolean', group: 'file', preview: true }),
	out('logging.file_folder', { group: 'file', advanced: true }),
	out('logging.file_size_mib', { kind: 'integer', group: 'file', advanced: true, preview: true }),
	out('logging.event_log_errors', { kind: 'boolean', group: 'event_log', advanced: true })
];

let server: ReturnType<typeof fakeServer>;
let form: SettingsForm;
async function start(locked = false) {
	server = fakeServer([
		values('logging', 1, {
			verbosity: locked
				? sv('normal', 'env', { env: 'LODESTAR_LOG', read_only: true })
				: sv('normal'),
			console_format: sv('text'),
			file_enabled: sv(true),
			file_folder: sv('/data/logs'),
			file_size_mib: sv(16),
			event_log_errors: sv(true)
		})
	]);
	vi.stubGlobal('fetch', server.fetch);
	form = createSettingsForm({ id: 'logging-test', fields: SPECS, href: (k) => `#${k}` });
	await until(() => expect(form.status).toBe('ready'));
}
const previews = () =>
	server.calls.filter((c) => c.path.endsWith('/settings/preview')).map((c) => c.body);

beforeEach(async () => {
	initLocale('en');
	await loadMessages(['logging']);
});
afterEach(() => {
	form?.destroy();
	vi.unstubAllGlobals();
});

describe('outputs', () => {
	it('shows the Windows Event Log setting only on a Windows service, without a disclosure', async () => {
		await start();
		const { rerender } = render(Outputs, { form, deployment: 'docker' });
		await screen.findByRole('heading', { name: 'Log file' });
		expect(screen.queryByRole('heading', { name: 'Windows Event Log' })).toBeNull();
		await rerender({ form, deployment: 'windows_service' });
		await screen.findByRole('heading', { name: 'Windows Event Log' });
		expect(screen.getByRole('switch', { name: 'Errors to the Windows Event Log' })).toBeTruthy();
		await rerender({ form, deployment: undefined });
		await until(() =>
			expect(screen.queryByRole('heading', { name: 'Windows Event Log' })).toBeNull()
		);
	});

	it('opens Advanced when one of its fields has an error, so the error is seen', async () => {
		await start();
		render(Outputs, { form });
		const details = (await screen.findByText('Advanced (2)')).closest('details')!;
		expect(details.open).toBe(false);
		(form as unknown as { setFieldError(k: string, e: string): void }).setFieldError(
			'logging.file_folder',
			'Must be a full path'
		);
		await until(() => expect(details.open).toBe(true));
	});
});

describe('preview', () => {
	it('follows staged changes, and leaves Verbosity out while the environment sets it', async () => {
		await start(true);
		render(Preview, { form });
		await until(() => expect(previews()).toEqual([{ logging: { file_enabled: true } }]));
		form.set('logging.file_size_mib', 32);
		await until(() => expect(previews().at(-1)).toEqual({ logging: { file_size_mib: 32 } }));
	});
});
