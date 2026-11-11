import { expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import ReviewList from './ReviewList.svelte';

const form = vi.hoisted(() => ({
	getSection: vi.fn(),
	spec: (key: string) =>
		key === 'crawler.nothing' ? undefined : { key, writeOnly: key === 'security.secret' }
}));
vi.mock('../settings/form/schema', () => ({
	loadSettingsIndex: async () => ({ spec: form.spec })
}));
vi.mock('../settings/form/messages', () => ({
	loadMessages: async () => {},
	fieldLabel: (spec: { key: string }) => `Label of ${spec.key}`
}));
vi.mock('../settings/form/display', () => ({
	displayValue: (_spec: unknown, value: unknown) => `<${String(value)}>`
}));
vi.mock('../settings/form/requests', () => ({ getSection: form.getSection }));

const until = (check: () => void) => waitFor(check, { timeout: 10_000 });

it('lists each pending field with its saved value, and never a secret', async () => {
	form.getSection.mockImplementation(async (section: string) => ({
		section,
		fields: {
			dht_port: { value: 6881 },
			secret: { value: 'hunter2' }
		}
	}));
	render(ReviewList, { fields: ['network.dht_port', 'security.secret'] });
	await until(() => expect(screen.getAllByTestId('restart-row')).toHaveLength(2));
	const [port, secret] = screen.getAllByTestId('restart-row');
	expect(port!.textContent).toContain('Label of network.dht_port');
	expect(port!.textContent).toContain('Saved: <6881>');
	expect(port!.textContent).toContain('applies after restart');
	expect(secret!.textContent).toContain('Saved: set');
	expect(document.body.textContent).not.toContain('hunter2');
});

it('says so when the changes cannot be loaded', async () => {
	form.getSection.mockRejectedValue(new Error('offline'));
	render(ReviewList, { fields: ['network.dht_port'] });
	await until(() => expect(screen.getByText("The changes couldn't be loaded.")).toBeTruthy());
});
