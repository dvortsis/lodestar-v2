// The v0.6 sources gate's UI checks (SRC-G; architecture §18.7, §18.9): every consent class shows
// its state and what it needs, no switch turns a source on past its condition (nothing is sent
// until the dialog is answered), the server's refusal leaves the switch off and says so, and an
// unverified search result carries its badge. Against the OpenAPI mock. Name ends in
// `routes.test.ts` so the `core` project runs it. Waits are conditions only.
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import { darknet, dht, lime, nyaa, nyaaAcked, tracker } from '../src/lib/features/sources/fixtures';
import { body, hashOf, hits, server } from './search-results.fixtures';
import type { components } from '../src/lib/api/schema';

type Source = components['schemas']['Source'];
const expect = baseExpect.configure({ timeout: 10_000 });

/** The Sources screen with `list`, recording every PATCH, which `answer` replies to. */
function sourcesBackend(
	mock: unknown,
	list: Source[],
	answer: (body: Record<string, unknown>) => { status: number; body: unknown }
) {
	new SettingsBackend().attach(mock as MockLike);
	const sent: Record<string, unknown>[] = [];
	const on = (mock as { on(id: string, f: unknown): unknown }).on.bind(mock);
	on('listSources', () => ({ status: 200, body: { sources: list } }));
	on('patchSource', (req: { postDataJSON(): Record<string, unknown> }) => {
		const b = req.postDataJSON();
		sent.push(b);
		return answer(b);
	});
	return sent;
}

const refused = {
	status: 400,
	body: {
		error: {
			code: 'invalid_setting',
			field: 'enabled',
			message: "this source asks for your consent before it's turned on, so it stays off."
		}
	}
};

test('every consent class shows its state, and no switch turns it on unasked', async ({
	page,
	go,
	mock
}) => {
	const sent = sourcesBackend(mock, [dht, nyaa, lime, darknet, tracker], () => refused);
	await go('/settings/sources');
	const classes: [Source, string][] = [
		[nyaa, 'External'],
		[lime, 'Restricted'],
		[darknet, 'Overlay'],
		[tracker, 'Account']
	];
	for (const [s, label] of classes) {
		const id = `source-${s.id}`;
		await expect(page.getByTestId(`${id}-consent`)).toHaveText(label);
		await expect(page.getByTestId(`${id}-status`)).toContainText('Needs setup');
		await expect(page.getByTestId(`${id}-need`)).toBeVisible();
		const toggle = page.getByRole('switch', { name: `${s.name}: on` });
		await expect(toggle).not.toBeChecked();
		await toggle.focus();
		await page.keyboard.press('Space');
		// The class's dialog asks first; leaving it sends nothing and the switch stays off.
		const dialog = page.getByRole('dialog');
		await expect(dialog).toBeVisible();
		await expect(toggle).not.toBeChecked();
		await page.keyboard.press('Escape');
		await expect(dialog).toBeHidden();
		await expect(toggle).not.toBeChecked();
	}
	// The class that needs nothing has no consent badge and runs.
	await expect(page.getByTestId('source-0-consent')).toHaveCount(0);
	await expect(page.getByTestId('source-0-status')).toContainText('Running');
	expect(sent).toEqual([]);
});

test("the server's refusal leaves the switch off and says nothing changed", async ({
	page,
	go,
	mock
}) => {
	// Acknowledged by an earlier build: the screen asks the server, and this build refuses.
	const sent = sourcesBackend(mock, [dht, nyaaAcked], () => refused);
	await go('/settings/sources');
	const toggle = page.getByRole('switch', { name: 'Nyaa: on' });
	await expect(toggle).not.toBeChecked();
	await toggle.focus();
	await page.keyboard.press('Space');
	await expect(page.getByTestId('sources-problem')).toContainText('Nothing was changed.');
	await expect(toggle).not.toBeChecked();
	expect(sent).toEqual([{ enabled: true }]);
});

test('an unverified result carries its badge and a verified one does not', async ({
	page,
	go,
	mock
}) => {
	const [verified, unverified] = hits(2);
	server(mock, () =>
		body([
			{ ...verified, verified: true },
			{ ...unverified, verified: false }
		])
	);
	await go('/search?q=lantern');
	const rows = (n: number) => page.getByTestId(`result-${hashOf(n)}`);
	await expect(rows(2).getByTestId('unverified-badge')).toHaveText('Unverified');
	await expect(rows(1)).toBeVisible();
	await expect(rows(1).getByTestId('unverified-badge')).toHaveCount(0);
});
