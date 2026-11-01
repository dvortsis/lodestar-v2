// The smoke mock for /settings/general serves the settings index and a General section, so its
// baselines show a real field. Name ends in `routes.test.ts`: the `core` project runs it.
import { mockSettings } from './support/settings-fixture';
import { expect as baseExpect, test } from './support/test';

const expect = baseExpect.configure({ timeout: 10_000 });

test('the smoke fixture renders the General section’s Data directory field', async ({
	page,
	go,
	mock
}) => {
	mockSettings(mock);
	await go('/settings/general');
	await expect(page.getByRole('textbox', { name: 'Data directory' })).toHaveValue('/data');
	expect(mock.calls('listSettings').length).toBeGreaterThan(0);
});
