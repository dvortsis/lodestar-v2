// Settings → Outputs (OUT-2) against the OpenAPI mock, with axe: a Torznab key is created behind
// the password dialog, sends `torznab` alone, and its secret shows once in Security's reveal
// dialog and is nowhere once that closes; the bitmagnet export starts a download of the bitmagnet
// format and shows the job's progress. Waits are conditions only. (Named `.routes.test.ts` so the
// `core` project picks it up.)
import { expectNoA11yViolations } from './a11y';
import { dialogSettled } from './support/dialog';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Index = components['schemas']['SettingsIndex'];
type Values = components['schemas']['SectionValues'];
type Key = components['schemas']['KeyInfo'];

const PASSWORD = 'current-password-1';
const SECRET = 'ls_torznab_0123456789abcdef';
const index = { sections: [] } as unknown as Index;
const values: Values[] = [
	{
		section: 'content',
		revision: 1,
		restart_required: [],
		fields: {
			export_adult: { value: false, env: null, read_only: false, source: 'default' }
		}
	} as unknown as Values
];
const key = (id: number, name: string): Key => ({
	id,
	name,
	kind: 'api_key',
	scopes: ['torznab'],
	status: 'active',
	prefix: `ls_${id}`,
	created_at: 1_700_000_000,
	last_used_at: null,
	expires_at: null,
	revoked_at: null,
	saved_search_id: null
});

test.describe('Settings → Outputs', () => {
	let sent: { op: string; body: unknown }[];
	/** Set once the download starts: until then `/jobs` holds no export, as on a fresh server. */
	let exporting: boolean;
	/** While set, the screen's first `/jobs` read (its mount read) waits for it: a slow first answer. */
	let holdFirstRead: Promise<void> | null;
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend(index, values).attach(mock as unknown as MockLike);
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		sent = [];
		exporting = false;
		holdFirstRead = null;
		on('listKeys', { keys: [key(1, 'Prowlarr')] });
		on('createKey', (r: { postDataJSON(): Record<string, unknown> }) => {
			const body = r.postDataJSON();
			sent.push({ op: 'createKey', body });
			return body.password === PASSWORD
				? { status: 200, body: { key: key(2, String(body.name)), secret: SECRET } }
				: {
						status: 401,
						body: {
							error: { code: 'bad_password', message: 'That password is wrong.', field: 'password' }
						}
					};
		});
		// A download is a navigation, which the mock doesn't answer; the job is what the screen shows.
		on('listJobs', async () => {
			if (holdFirstRead) {
				const held = holdFirstRead;
				holdFirstRead = null;
				await held;
			}
			// What the server holds when it answers, not when it was asked.
			const at = Math.floor(Date.now() / 1000);
			if (!exporting) return { jobs: [] };
			return {
				jobs: [
					{
						id: 5,
						kind: 'export',
						state: 'done',
						started_at: at,
						finished_at: at,
						done: 42,
						skipped: 0,
						total: 42,
						total_approx: false,
						rate: null,
						eta_s: null,
						export: { cap: 1_000_000, format: 'bitmagnet', truncated: false }
					}
				]
			};
		});
	});

	test('creates a Torznab key behind the password; its secret shows once', async ({ page, go }) => {
		await go('/settings/outputs');
		await expect(page.getByTestId('outputs-torznab-url')).toHaveValue(/\/api\/v1\/torznab$/);
		await expect(page.getByText('Prowlarr', { exact: true })).toBeVisible();
		await expect(page.getByRole('textbox', { name: /App/ })).toHaveValue('Sonarr');
		await expectNoA11yViolations(page);

		await page.getByTestId('outputs-key-create').click();
		// The Create button spins while the dialog is open, so wait for focus, as Security's spec does.
		const dialog = page.getByRole('dialog', { name: 'Create a Torznab key for Sonarr' });
		await expect(dialog.getByLabel('Your password')).toBeFocused();
		await page.keyboard.type(PASSWORD);
		await page.keyboard.press('Enter');

		const reveal = page.getByRole('dialog', { name: 'Copy the secret for Sonarr' });
		await dialogSettled(page, 'Copy the secret for Sonarr');
		await expect(reveal.getByTestId('key-secret')).toHaveValue(SECRET);
		await expect(reveal).toContainText('This is the only time Lodestar shows it');
		await expectNoA11yViolations(page);
		expect(sent).toEqual([
			{
				op: 'createKey',
				body: { name: 'Sonarr', kind: 'api_key', scopes: ['torznab'], password: PASSWORD }
			}
		]);

		await reveal.getByTestId('key-secret-done').click();
		await expect(reveal).toBeHidden();
		await expect(page.getByText('Created a key for Sonarr.')).toBeVisible();
		// Gone from the page, storage and the address once the dialog has closed.
		await expect
			.poll(() =>
				page.evaluate(
					(s) =>
						[
							document.documentElement.outerHTML,
							JSON.stringify({ ...localStorage }),
							JSON.stringify({ ...sessionStorage }),
							location.href
						].some((t) => t.includes(s)),
					SECRET
				)
			)
			.toBe(false);
	});

	// The screen reads `/jobs` once when it opens, to learn the ids already taken, and a press then
	// looks for a job above the highest. The mock lists the export's job only once `exporting` is
	// set, so the test must not set it until that first read has been answered: a late answer would
	// otherwise already list the job, and the press would wait for an id above it (the CI failure
	// of #1031). A real server creates the job when the download starts, after the press, so the
	// screen needs no change. `data-ids-known` on the Start link is the screen's own signal that the
	// read has answered. `slow` holds that first answer until a condition (the page has rendered),
	// the race the test used to lose when the mock was slow to answer.
	for (const slow of [false, true]) {
		test(`starts the bitmagnet export and shows its progress${slow ? ' (slow first /jobs read)' : ''}`, async ({
			page,
			go
		}) => {
			let release!: () => void;
			if (slow) holdFirstRead = new Promise<void>((resolve) => (release = resolve));
			await go('/settings/outputs');
			await expect(page.getByTestId('outputs-bitmagnet-left-out')).toContainText('hidden torrents');
			// The page is up, and only now does the slow read answer.
			if (slow) release();
			const start = page.getByTestId('outputs-bitmagnet-start');
			await expect(start).toHaveAttribute('data-ids-known', 'true');
			const download = page.waitForEvent('download');
			exporting = true;
			await start.click();
			expect((await download).url()).toMatch(/\/api\/v1\/export\?format=bitmagnet$/);
			await expect(page.getByTestId('outputs-bitmagnet-count')).toHaveText(
				'Done: 42 torrents in the file.'
			);
			// The example command wraps instead of scrolling, between words where it can: nothing of it is hidden at any width.
			const curl = page.getByTestId('outputs-bitmagnet-curl');
			await expect(curl).toContainText('http://<bitmagnet-host>:3333/import');
			for (const width of [320, 360, 768, 1366]) {
				await page.setViewportSize({ width, height: 900 });
				await expect.poll(() => curl.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
			}
			// Nor does the page scroll sideways on a phone.
			await page.setViewportSize({ width: 360, height: 900 });
			await expect
				.poll(() =>
					page.evaluate(
						() => document.documentElement.scrollWidth <= document.documentElement.clientWidth
					)
				)
				.toBe(true);
			await expectNoA11yViolations(page);
		});
	}
});
