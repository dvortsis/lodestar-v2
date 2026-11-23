// The palette button (X-shell-home, review P5): the keys it answers to on a keyboard device, a
// magnifier on touch, and an accessible name that contains the visible text (WCAG 2.5.3).
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/svelte';

vi.mock('$app/navigation', () => ({ afterNavigate: () => {}, goto: vi.fn() }));
vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));
vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/search') } }));
const platform = vi.hoisted(() => ({ mac: false }));
vi.mock('$lib/keys/combo', async (original) => ({
	...(await original<typeof import('$lib/keys/combo')>()),
	isMacPlatform: () => platform.mac
}));
import TopBar from './TopBar.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

async function open(touch: boolean, mac: boolean) {
	platform.mac = mac;
	vi.stubGlobal('matchMedia', (query: string) => ({
		matches: touch && query === '(pointer: coarse)',
		media: query,
		addEventListener: () => {},
		removeEventListener: () => {}
	}));
	render(TopBar);
	return screen.getByTestId('palette-button');
}

describe('TopBar palette button', () => {
	it('shows Ctrl+K on a keyboard device, and the name carries the visible text', async () => {
		const button = await open(false, false);
		expect(button.textContent?.trim()).toBe('Ctrl+K');
		expect(button.getAttribute('aria-label')).toBe('Search and commands Ctrl+K');
		expect(button.getAttribute('aria-keyshortcuts')).toBe('Control+K');
	});

	it('shows ⌘K on Apple platforms', async () => {
		const button = await open(false, true);
		expect(button.textContent?.trim()).toBe('⌘K');
		expect(button.getAttribute('aria-label')).toBe('Search and commands ⌘K');
		expect(button.getAttribute('aria-keyshortcuts')).toBe('Meta+K');
	});

	it('shows a magnifier, no keys, on a touch device', async () => {
		const button = await open(true, false);
		expect(button.textContent?.trim()).toBe('');
		expect(button.querySelector('svg')).not.toBeNull();
		expect(button.getAttribute('aria-label')).toBe('Search and commands');
	});
});
