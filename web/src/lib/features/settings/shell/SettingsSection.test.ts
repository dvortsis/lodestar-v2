import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import type { Component } from 'svelte';
import SettingsSection from './SettingsSection.svelte';
import Stub from './testing/Stub.svelte';
import type { About, Screen } from './screens';

// A fake registry: one hand-built screen, one generated screen with an extra.
const registry = vi.hoisted(() => ({
	load: undefined as unknown as () => Promise<{ default: Component }>,
	extra: undefined as unknown as () => Promise<{ default: Component }>,
	about: undefined as unknown as () => Promise<Record<string, About>>,
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
			about: () => registry.about(),
			sections: ['storage'],
			extra: () => registry.extra()
		},
		// Opened by one test each, so `about` loads cold (loaded ones are kept for the module).
		...['fail', 'late'].map((id) => ({
			id,
			group: 'library' as const,
			title: () => id,
			about: () => registry.about(),
			sections: ['storage']
		})),
		{
			id: 'net',
			group: 'crawling',
			title: () => 'Net',
			about: () => registry.about(),
			sections: ['network']
		}
	] satisfies Screen[],
	screenFields: () => [{ key: 'storage.mmap_mib', section: 'storage' }],
	aboutOf: async (s: Screen) => (await s.about?.())?.[s.id] ?? {}
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
	registry.about = () =>
		Promise.resolve({
			disk: { purpose: () => 'Disk purpose', notice: () => 'Read-only for now' },
			net: { purpose: () => 'Net purpose' }
		});
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

it('waits for the purpose line with the form, and says so when it cannot load', async () => {
	const pending = deferred<Record<string, About>>();
	registry.about = () => pending.promise;
	render(SettingsSection, { section: 'fail' });
	await until(() => expect(screen.getByRole('status').textContent).toContain('Loading settings'));
	expect(screen.queryByTestId('stub')).toBeNull();
	// The purpose line's height is kept while it loads, ahead of the status line.
	const line = document.querySelector('p.h-6');
	expect(line?.nextElementSibling).toBe(screen.getByRole('status'));
	pending.reject(new Error('chunk'));
	await screen.findByRole('button', { name: 'Reload' }, { timeout: 10_000 });
	expect(screen.queryByText('Fail purpose')).toBeNull();
});

it('puts the purpose in the line it kept, without the reserved height', async () => {
	const pending = deferred<Record<string, About>>();
	registry.about = () => pending.promise;
	render(SettingsSection, { section: 'net' });
	const line = await waitFor(() => document.querySelector('p.h-6') as HTMLElement, {
		timeout: 10_000
	});
	pending.resolve({ net: { purpose: () => 'Net purpose' } });
	await until(() => expect(line.textContent).toBe('Net purpose'));
	expect(line.classList.contains('h-6')).toBe(false);
});

it('never shows a purpose line that arrives after leaving its screen', async () => {
	const slow = deferred<Record<string, About>>();
	const ready = registry.about;
	registry.about = () => slow.promise;
	const view = render(SettingsSection, { section: 'late' });
	registry.about = ready;
	await view.rerender({ section: 'net' });
	await until(() => expect(screen.getByText('Net purpose')).toBeTruthy());
	slow.resolve({ late: { purpose: () => 'Late purpose' }, net: { purpose: () => 'Stale' } });
	await until(() => expect(screen.getByTestId('stub')).toBeTruthy());
	expect(screen.queryByText('Late purpose')).toBeNull();
	expect(screen.queryByText('Stale')).toBeNull();
	expect(screen.getByRole('heading', { level: 1, name: 'Net' })).toBeTruthy();
});

it('fetches the settings index once, even as the form’s code arrives', async () => {
	const index = vi.fn(() => Promise.resolve({}));
	registry.index = index;
	render(SettingsSection, { section: 'disk' });
	await until(() => expect(screen.getByText('Export panel')).toBeTruthy());
	expect(index).toHaveBeenCalledOnce();
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
