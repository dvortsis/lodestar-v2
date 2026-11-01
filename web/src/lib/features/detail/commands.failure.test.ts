import { beforeEach, expect, it, vi } from 'vitest';
import { toasts } from '$lib/ui/feedback/toasts.svelte';

// The magnet chunk fails to load: the shortcut says so instead of staying silent.
vi.mock('./magnet', () => {
	throw new Error('chunk failed');
});
vi.mock('$lib/keys', async (original) => {
	const real = await original<typeof import('$lib/keys')>();
	return { ...real, keys: real.createKeys({ mac: false }) };
});

const { keys } = await import('$lib/keys');
const commands = await import('./commands');

const press = (key: string) =>
	keys.handle(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));

beforeEach(() => {
	toasts.items = [];
});

it('toasts when the magnet chunk fails to load for c and for o', async () => {
	commands.setCommandTarget('a'.repeat(40));
	await commands.registerDetailCommands();
	press('c');
	await vi.waitFor(() => expect(toasts.items).toHaveLength(1), { timeout: 10_000 });
	press('o');
	await vi.waitFor(() => expect(toasts.items).toHaveLength(2), { timeout: 10_000 });
	expect(toasts.items.map((t) => [t.kind, t.text])).toEqual([
		['error', "Couldn't get the magnet link."],
		['error', "Couldn't get the magnet link."]
	]);
});
