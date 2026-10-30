import { expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import SettingsSection from './SettingsSection.svelte';
import type { Screen } from './screens';

// A generated screen whose form code can't load: the page says so and offers Reload.
vi.mock('./screens', () => ({
	screens: [
		{ id: 'disk', group: 'library', title: () => 'Disk', sections: ['storage'] }
	] satisfies Screen[],
	screenFields: () => [{ key: 'storage.mmap_mib', section: 'storage' }]
}));
vi.mock('../form/schema', () => ({ loadSettingsIndex: () => Promise.resolve({}) }));
vi.mock('./SectionForm.svelte', () => {
	throw new Error('chunk');
});
vi.setConfig({ testTimeout: 15_000 });

it('offers Reload when a generated screen’s form cannot load', async () => {
	render(SettingsSection, { section: 'disk' });
	await screen.findByRole('button', { name: 'Reload' }, { timeout: 10_000 });
	expect(screen.getByRole('heading', { level: 1, name: 'Disk' })).toBeTruthy();
	expect(screen.queryByRole('status')).toBeNull();
});
