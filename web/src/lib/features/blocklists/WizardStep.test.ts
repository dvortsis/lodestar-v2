import { afterEach, describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { mockFetch } from '../filters/flows/testing/fetch';
import { applied, finished, strengthening, weakening } from '../filters/flows/testing/fixtures';
import WizardStep from './WizardStep.svelte';
import { catalog, strictness, subscriptions } from './fixtures';

configure({ defaultHidden: true, asyncUtilTimeout: 10_000 });
vi.setConfig({ testTimeout: 30_000 });

let net: ReturnType<typeof mockFetch>;
function setup(subs = subscriptions()) {
	net = mockFetch();
	net.on('GET /filters/subscriptions', { body: subs });
	net.on('POST /filters/strictness', {
		body: strictness({ level: 'severe', text: 'Severe: blocks a third of the internet.' })
	});
	net.on('POST /filters/preview/jobs', { status: 202, body: { job: 7 } });
	net.on('POST /filters/preview/jobs/{id}/cancel', { body: { ok: true } });
	return render(WizardStep);
}
const wait = (check: () => void) => waitFor(check, { timeout: 10_000 });
afterEach(() => vi.unstubAllGlobals());

describe('wizard blocklist step', () => {
	it('pre-selects nothing, and connects nothing when Continue is pressed with no choice', async () => {
		const { component } = setup();
		const box = (await screen.findByTestId('wizard-blocklist-bt_blocklists')) as HTMLInputElement;
		expect(box.checked).toBe(false);
		expect(screen.getByTestId('wizard-blocklists-none')).toBeTruthy();
		expect(await component.save()).toBe(true);
		expect(net.sent.filter((s) => s.method !== 'GET')).toHaveLength(0);
	});

	it('shows the strictness warning for the choice, then connects it through a preview', async () => {
		const { component } = setup();
		await fireEvent.click(await screen.findByTestId('wizard-blocklist-bt_blocklists'));
		await wait(() => expect(screen.getByTestId('blocklists-level').textContent).toBe('Severe'));
		expect(JSON.parse(net.to('/filters/strictness')[0]!.body!)).toEqual({
			catalog: ['bt_blocklists'],
			subscriptions: []
		});
		const a = strengthening();
		net.on('GET /filters/preview/jobs/{id}', { body: finished(a) });
		net.on('POST /filters/apply', { body: applied(a, { id: 9 }) });
		expect(await component.save()).toBe(true);
		const apply = JSON.parse(net.to('/filters/apply')[0]!.body!);
		expect(apply.change).toMatchObject({
			op: 'subscribe',
			url: 'https://lists.example.org/bt.p2p.gz'
		});
		expect(apply.password).toBeUndefined();
	});

	it('connects nothing when the server wants a confirmation, and points to Settings', async () => {
		const { component } = setup();
		await fireEvent.click(await screen.findByTestId('wizard-blocklist-bt_blocklists'));
		await wait(() => expect(screen.getByTestId('blocklists-level')).toBeTruthy());
		net.on('GET /filters/preview/jobs/{id}', { body: finished(weakening()) });
		expect(await component.save()).toBe(false);
		expect(net.to('/filters/apply')).toHaveLength(0);
		expect(screen.getByTestId('wizard-blocklists-error').textContent).toContain('Settings');
	});

	it('does not offer a list that is already connected', async () => {
		setup(subscriptions({ catalog: [catalog({ subscription: 3 })] }));
		const box = (await screen.findByTestId('wizard-blocklist-bt_blocklists')) as HTMLInputElement;
		expect(box.disabled).toBe(true);
	});
});
