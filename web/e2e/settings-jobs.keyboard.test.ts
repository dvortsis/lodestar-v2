// Settings → Jobs (SRC-16) against the OpenAPI mock, by keyboard alone, with axe: progress, rate and
// ETA come from the server; Pause, Resume and Cancel change the row only to what the server answers.
// Waits are conditions only.
import { expectNoA11yViolations } from './a11y';
import { expect as baseExpect, test } from './support/test';
import {
	SettingsBackend,
	type MockLike
} from '../src/lib/features/settings/form/testing/e2e-backend';
import type { components } from '../src/lib/api/schema';

const expect = baseExpect.configure({ timeout: 10_000 });
type Job = components['schemas']['BackgroundJob'];

const job = (over: Partial<Job> = {}): Job => ({
	done: 400,
	eta_s: 120,
	finished_at: null,
	id: 7,
	kind: 'export',
	rate: 20,
	skipped: 100,
	started_at: 1_699_999_000,
	state: 'running',
	total: 1000,
	total_approx: false,
	...over
});

test.describe('keyboard: Settings → Jobs', () => {
	let jobs: Job[];
	let sent: string[];
	test.beforeEach(async ({ mock }) => {
		new SettingsBackend().attach(mock as unknown as MockLike);
		jobs = [job(), job({ id: 6, state: 'done', finished_at: 1_699_999_500, eta_s: null })];
		sent = [];
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listJobs', () => ({ status: 200, body: { jobs } }));
		for (const [op, to] of [
			['pauseJob', 'paused'],
			['resumeJob', 'running'],
			['cancelJob', 'cancelled']
		] as const)
			on(op, () => {
				sent.push(op);
				jobs = jobs.map((j) => (j.id === 7 ? { ...j, state: to } : j));
				return { status: 200, body: jobs.find((j) => j.id === 7) };
			});
	});

	test('shows progress, rate and ETA, and passes axe', async ({ page, go }) => {
		await go('/settings/jobs');
		await expect(page.getByRole('heading', { level: 1, name: 'Jobs' })).toBeVisible();
		await expect(page.getByTestId('job-7-state')).toHaveText('Running');
		await expect(page.getByTestId('job-7-progress')).toContainText('500 of 1,000');
		await expect(page.getByTestId('job-7-rate')).toHaveText('20 items/s');
		await expect(page.getByTestId('job-7-eta')).toHaveText('2 min');
		await expect(page.getByTestId('job-6-state')).toHaveText('Done');
		await expectNoA11yViolations(page);
	});

	test('pauses, resumes and cancels by keyboard', async ({ page, go, kbd }) => {
		await go('/settings/jobs');
		await kbd.tabTo(page.getByTestId('job-7-pause'), 200);
		await kbd.press('Enter');
		await expect(page.getByTestId('job-7-state')).toHaveText('Paused');
		await expect(page.getByTestId('jobs-announce')).toHaveText('Export #7: Paused');
		// Focus stays in the row, on the control that replaced the one pressed.
		await expect(page.getByTestId('job-7-resume')).toBeFocused();
		await kbd.press('Enter');
		await expect(page.getByTestId('job-7-state')).toHaveText('Running');
		await kbd.tabTo(page.getByTestId('job-7-cancel'), 20);
		await kbd.press('Enter');
		await expect(page.getByTestId('job-7-state')).toHaveText('Cancelling');
		await expect(page.getByTestId('job-7-cancel')).toHaveCount(0);
		expect(sent).toEqual(['pauseJob', 'resumeJob', 'cancelJob']);
		await expectNoA11yViolations(page);
	});

	test('shows the empty state', async ({ page, go, mock }) => {
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listJobs', () => ({ status: 200, body: { jobs: [] } }));
		await go('/settings/jobs');
		await expect(page.getByTestId('jobs-empty')).toBeVisible();
		await expectNoA11yViolations(page);
	});

	test('shows a load error and recovers on retry', async ({ page, go, mock, kbd }) => {
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('listJobs', () => ({
			status: 500,
			body: { error: { code: 'internal', message: 'boom' } }
		}));
		await go('/settings/jobs');
		await expect(page.getByTestId('jobs-retry')).toBeVisible();
		await expectNoA11yViolations(page);
		on('listJobs', () => ({ status: 200, body: { jobs } }));
		await kbd.tabTo(page.getByTestId('jobs-retry'), 200);
		await kbd.press('Enter');
		await expect(page.getByTestId('job-7')).toBeVisible();
	});
});
