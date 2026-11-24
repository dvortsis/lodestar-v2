// Settings → Add torrent files (UP-2) against the OpenAPI mock: files
// are sent as the raw body with the fixed content type, results show per file with Retry, the watch
// folder saves on the form engine and names the data-folder rule beside it, and axe passes.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import { SETTINGS_INDEX } from './support/settings-fixture';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];

const index = {
	sections: SETTINGS_INDEX.sections.filter((s) => s.section === 'watch' || s.section === 'general')
} as Index;
const v = (value: unknown, over: Partial<components['schemas']['SettingValue']> = {}) => ({
	value,
	env: null,
	read_only: false,
	restart_required: false,
	source: 'default' as const,
	...over
});
const values = (): Values[] => [
	{
		section: 'watch',
		revision: 2,
		restart_required: [],
		fields: { folder: v(''), after: v('move') }
	},
	{
		section: 'general',
		revision: 1,
		restart_required: [],
		fields: {
			data_dir: v('/data', {
				source: 'env',
				read_only: true,
				restart_required: true,
				env: 'LODESTAR_DATA_DIR'
			})
		}
	}
];
const torrent = (name: string) => ({
	name,
	mimeType: 'application/x-bittorrent',
	buffer: Buffer.from('d4:infod4:name1:aee')
});

test('uploads files one by one, shows each result and retries a failed one', async ({
	page,
	go,
	mock
}) => {
	new SettingsBackend(index, values()).attach(mock as unknown as MockLike);
	const sent: { type: string | null; names: boolean }[] = [];
	const answers = [
		{ status: 202, body: { info_hash: 'a'.repeat(40), private: false } },
		{ status: 422, body: { error: { code: 'held', message: 'held' } } },
		{
			status: 429,
			body: { error: { code: 'rate_limited', message: 'slow down' } },
			headers: { 'Retry-After': '3' }
		},
		{ status: 202, body: { info_hash: 'b'.repeat(40), private: true } }
	];
	mock.on('uploadTorrent', (request) => {
		const body = request.postDataBuffer()?.toString() ?? '';
		sent.push({
			type: request.headers()['content-type'] ?? null,
			names: body.includes('.torrent')
		});
		return answers.shift()!;
	});
	await go('/settings/import');
	await expect(page.getByRole('heading', { level: 1, name: 'Add torrent files' })).toBeVisible();
	await expect(page.getByTestId('upload-limits')).toContainText('Up to 30 files at a time');

	await page
		.getByTestId('upload-input')
		.setInputFiles([torrent('one.torrent'), torrent('two.torrent'), torrent('three.torrent')]);
	const items = page.getByTestId('upload-item');
	await expect(items).toHaveCount(3);
	await expect(page.getByTestId('upload-item-state')).toHaveText([
		'Added',
		'Held by filters',
		'Too many uploads. Retry in 3 s'
	]);
	expect(sent).toEqual([
		{ type: 'application/x-bittorrent', names: false },
		{ type: 'application/x-bittorrent', names: false },
		{ type: 'application/x-bittorrent', names: false }
	]);
	await expect(page.getByTestId('upload-open')).toHaveAttribute('href', `/t/${'a'.repeat(40)}`);
	await expectNoA11yViolations(page);

	await page.getByRole('button', { name: 'Retry three.torrent' }).click();
	await expect(items.nth(2)).toHaveAttribute('data-state', 'added');
	await expect(items.nth(2)).toContainText('Added, private');
	await expect(page.getByTestId('upload-count-added')).toHaveText('2 added');
});

test('the watch folder saves, and says when it is inside the data folder', async ({
	page,
	go,
	mock
}) => {
	const backend = new SettingsBackend(index, values()).attach(mock as unknown as MockLike);
	await go('/settings/import');
	const folder = page.getByRole('textbox', { name: 'Folder to watch' });
	await expect(folder).toBeVisible();
	await expect(page.getByTestId('watch-problem')).toHaveCount(0);
	await folder.fill('/data/watch');
	await expect(page.getByTestId('watch-problem')).toContainText(
		"Off: this folder is Lodestar's data folder or inside it"
	);
	await folder.fill('/watch');
	await expect(page.getByTestId('watch-problem')).toHaveCount(0);
	await page.getByRole('button', { name: 'Save changes' }).click();
	await expect.poll(() => backend.patches.length).toBe(1);
	expect(backend.patches[0]).toEqual({ watch: { if_revision: 2, folder: '/watch' } });
	await expectNoA11yViolations(page);
});
