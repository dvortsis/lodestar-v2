// If the companion chip's code can't be fetched (a stale tab after an upgrade), the row says
// "Needs setup" as a plain note instead of dropping the need, and nothing is left unhandled (CMP-9b).
import { describe, expect, it, vi } from 'vitest';
import { configure, render, screen } from '@testing-library/svelte';
import SourceRow from './SourceRow.svelte';
import { source } from './fixtures';

configure({ asyncUtilTimeout: 10_000 });

vi.mock('../companions/NeedChip.svelte', () => {
	throw new Error('Failed to fetch dynamically imported module');
});

describe('the chip fails to load', () => {
	it('shows a plain "Needs setup" note with no Set up button', async () => {
		const waiting = source({
			id: 5,
			name: 'Download client',
			companion_need: { companion: 'qbittorrent', state: 'missing' }
		});
		render(SourceRow, {
			source: waiting,
			busy: false,
			onenable: async () => true,
			onpriority: async () => true,
			onsetup: () => {},
			onpurge: () => {},
			onsettings: () => {}
		});
		const note = await screen.findByTestId('source-5-need-fallback');
		expect(note.textContent).toBe('Needs setup');
		expect(screen.queryByRole('button', { name: /Set up/ })).toBeNull();
	});
});
