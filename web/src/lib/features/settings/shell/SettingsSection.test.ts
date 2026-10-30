import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import type { Component } from 'svelte';
import SettingsSection from './SettingsSection.svelte';
import Stub from './testing/Stub.svelte';
import type { Screen } from './screens';

// A fake registry: one hand-built screen, one generated screen with an extra.
const registry = vi.hoisted(() => ({
	load: undefined as unknown as () => Promise<{ default: Component }>,
	extra: undefined as unknown as () => Promise<{ default: Component }>,
	index: undefined as unknown as () => Promise<unknown>
}));
vi.mock('./screens', () => ({
	screens: [
		{ id: 'keys', group: 'preferences', title: () => 'Keys', load: () => registry.load() },
		{ id: 'slow', group: 'preferences', title: () => 'Slow', load: () => registry.load() },
		{
			id: 'broken',
			group: 'preferences',
			title: () => 'Broken',
			load: () => Promise.reject(new Error('chunk'))
		},
		{
			id: 'disk',
			group: 'library',
			title: () => 'Disk',
			purpose: () => 'Disk purpose',
			sections: ['storage'],
			notice: () => 'Read-only for now',
			extra: () => registry.extra()
		}
	] satisfies Screen[],
	screenFields: () => [{ key: 'storage.mmap_mib', section: 'storage' }]
}));
vi.mock('../form/schema', () => ({ loadSettingsIndex: () => registry.index() }));
vi.mock('./SectionForm.svelte', async () => ({
	default: (await import('./testing/Stub.svelte')).default
}));

const until = (check: () => void) => waitFor(check, { timeout: 10_000 });
vi.setConfig({ testTimeout: 15_000 });
const body = (text: string, heading = true) => ({
	default: ((anchor: unknown, props: Record<string, unknown>) =>
		(Stub as unknown as (a: unknown, p: unknown) => unknown)(anchor, {
			...props,
			text,
			heading
		})) as unknown as Component
});
const deferred = <T>() => {
	let resolve!: (v: T) => void;
	let reject!: (e: unknown) => void;
	const promise = new Promise<T>((a, b) => ((resolve = a), (reject = b)));
	return { promise, resolve, reject };
};

beforeEach(() => {
	registry.load = () => Promise.resolve(body('Keyboard body'));
	registry.extra = () => Promise.resolve(body('Export panel', false));
	registry.index = () => Promise.resolve({});
});
afterEach(() => vi.restoreAllMocks());

it('titles a hand-built screen while it loads, then shows only its own body', async () => {
	const pending = deferred<{ default: Component }>();
	registry.load = () => pending.promise;
	render(SettingsSection, { section: 'slow' });
	expect(screen.getByRole('heading', { level: 1, name: 'Slow' })).toBeTruthy();
	expect(screen.getByRole('status').textContent).toContain('Loading settings');
	pending.resolve(body('Keyboard body'));
	await until(() =>
		expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Keyboard body')
	);
	expect(screen.getAllByRole('heading', { level: 1 })).toHaveLength(1);
	expect(screen.queryByRole('status')).toBeNull();
});

it('keeps the title and offers Reload when a hand-built screen cannot load', async () => {
	const reload = vi.fn();
	vi.spyOn(window, 'location', 'get').mockReturnValue({ ...window.location, reload });
	render(SettingsSection, { section: 'broken' });
	const button = await screen.findByRole('button', { name: 'Reload' }, { timeout: 10_000 });
	expect(screen.getByRole('heading', { level: 1, name: 'Broken' })).toBeTruthy();
	expect(document.body.textContent).toContain("Part of this page couldn't load");
	await fireEvent.click(button);
	expect(reload).toHaveBeenCalledOnce();
});

it('shows a generated screen with its purpose, notice, form and extra', async () => {
	render(SettingsSection, { section: 'disk' });
	await until(() => expect(screen.getByText('Export panel')).toBeTruthy());
	expect(screen.getByRole('heading', { level: 1, name: 'Disk' })).toBeTruthy();
	expect(screen.getByText('Disk purpose')).toBeTruthy();
	expect(screen.getByText('Read-only for now')).toBeTruthy();
	expect(screen.getAllByTestId('stub').map((el) => el.textContent)).toEqual([
		'Stub',
		'Export panel'
	]);
});

it('keeps the form when its extra cannot load, and says so', async () => {
	registry.extra = () => Promise.reject(new Error('chunk'));
	render(SettingsSection, { section: 'disk' });
	await screen.findByRole('button', { name: 'Reload' }, { timeout: 10_000 });
	expect(screen.getByTestId('stub').textContent).toBe('Stub');
});

it('renders a loaded body at once on a return visit', async () => {
	const first = render(SettingsSection, { section: 'keys' });
	await until(() => expect(screen.getByText('Keyboard body')).toBeTruthy());
	first.unmount();
	const load = vi.fn(() => Promise.resolve(body('Keyboard body')));
	registry.load = load;
	render(SettingsSection, { section: 'keys' });
	expect(screen.getByText('Keyboard body')).toBeTruthy();
	expect(screen.queryByRole('status')).toBeNull();
	expect(load).not.toHaveBeenCalled();
});

it('switches from a hand-built screen to a generated one', async () => {
	const view = render(SettingsSection, { section: 'keys' });
	await until(() => expect(screen.getByText('Keyboard body')).toBeTruthy());
	await view.rerender({ section: 'disk' });
	await until(() => expect(screen.getByText('Export panel')).toBeTruthy());
	expect(screen.queryByText('Keyboard body')).toBeNull();
	expect(screen.getByRole('heading', { level: 1, name: 'Disk' })).toBeTruthy();
});

it('names an unknown destination without loading anything', () => {
	const index = vi.fn(() => Promise.resolve({}));
	registry.index = index;
	render(SettingsSection, { section: 'nowhere' });
	expect(screen.getByRole('heading', { level: 1, name: 'Settings' })).toBeTruthy();
	expect(document.body.textContent).toContain('not available in this build');
	expect(index).not.toHaveBeenCalled();
});
