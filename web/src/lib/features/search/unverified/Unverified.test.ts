import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import UnverifiedBadge from './UnverifiedBadge.svelte';

const state = vi.hoisted(() => ({ ast: null as unknown, setAst: null as unknown }));
vi.mock('$app/navigation', () => ({ replaceState: vi.fn() }));
vi.mock('$app/state', () => ({ page: { url: new URL('http://x/search') } }));
vi.mock('$lib/state/search.svelte', () => ({
	search: {
		get ast() {
			return state.ast;
		},
		setAst: (...a: unknown[]) => (state.setAst as (...a: unknown[]) => Promise<void>)(...a)
	}
}));

import UnverifiedToggle from './UnverifiedToggle.svelte';

describe('UnverifiedBadge', () => {
	it('shows text and an icon', () => {
		render(UnverifiedBadge);
		const el = screen.getByTestId('unverified-badge');
		expect(el.textContent).toContain('Unverified');
		expect(el.querySelector('svg')).toBeTruthy();
	});
});

describe('UnverifiedToggle', () => {
	it('is off by default and adds verified:yes,no when checked', async () => {
		state.ast = { op: 'all' };
		const setAst = vi.fn().mockResolvedValue(undefined);
		state.setAst = setAst;
		render(UnverifiedToggle);
		const box = screen.getByRole('checkbox', { name: 'Show unverified' }) as HTMLInputElement;
		expect(box.checked).toBe(false);
		await fireEvent.click(box);
		expect(setAst).toHaveBeenCalledWith({ op: 'verified', any: ['yes', 'no'] });
	});
	it('is on when the query already shows them, and removes the clause when unchecked', async () => {
		state.ast = { op: 'verified', any: ['no'] };
		const setAst = vi.fn().mockResolvedValue(undefined);
		state.setAst = setAst;
		render(UnverifiedToggle);
		const box = screen.getByRole('checkbox', { name: 'Show unverified' }) as HTMLInputElement;
		expect(box.checked).toBe(true);
		await fireEvent.click(box);
		expect(setAst).toHaveBeenCalledWith({ op: 'all' });
	});
	it('goes back to the real state when the change fails', async () => {
		state.ast = { op: 'all' };
		state.setAst = vi.fn().mockRejectedValue(new Error('x'));
		render(UnverifiedToggle);
		const box = screen.getByRole('checkbox') as HTMLInputElement;
		await fireEvent.click(box);
		await vi.waitFor(() => expect(box.checked).toBe(false), { timeout: 10_000 });
	});
});
