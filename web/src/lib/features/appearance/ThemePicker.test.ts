import { describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen } from '@testing-library/svelte';
import ThemePicker from './ThemePicker.svelte';
import { DEFAULTS } from './prefs';
import type { ThemeEntry, ThemesFile } from './themes';

configure({ defaultHidden: true });

const e = (id: string, mode: 'light' | 'dark', family: string, group: string): ThemeEntry => ({
	id,
	name: id,
	family,
	group,
	mode,
	contrast: 'AA',
	values: ['#111111'],
	keys: ['--color-base-100']
});
const file: ThemesFile = {
	v: 1,
	keys: ['--color-base-100'],
	themes: [
		e('nord-light', 'light', 'Nord', 'community'),
		e('nord-dark', 'dark', 'Nord', 'community'),
		e('moss-light', 'light', 'Moss', 'original'),
		e('moss-dark', 'dark', 'Moss', 'original')
	]
};
const open = (over = {}) => {
	const onpick = vi.fn();
	render(ThemePicker, {
		prefs: DEFAULTS,
		dark: false,
		themes: async () => file,
		custom: () => [],
		write: vi.fn(),
		onpick,
		...over
	});
	return onpick;
};

describe('ThemePicker', () => {
	it('lists Riverstone and every family as one radio group, with text names', async () => {
		open();
		await screen.findByRole('radio', { name: /Nord/ });
		const group = await screen.findByRole('radiogroup', { name: 'Theme' });
		const names = (await screen.findAllByRole('radio')).map((r) => r.closest('label')!.textContent);
		expect(group).toBeTruthy();
		expect(names.some((n) => n?.includes('Riverstone'))).toBe(true);
		expect(names.some((n) => n?.includes('Nord'))).toBe(true);
		expect((screen.getByRole('radio', { name: /Riverstone/ }) as HTMLInputElement).checked).toBe(
			true
		);
	});

	it('picks a family by its id', async () => {
		const onpick = open();
		await fireEvent.click(await screen.findByRole('radio', { name: /Moss/ }));
		expect(onpick).toHaveBeenCalledWith({ theme: 'moss' });
	});

	it('filters by group', async () => {
		open();
		await screen.findByRole('radio', { name: /Nord/ });
		await fireEvent.change(screen.getByLabelText('Filter themes'), {
			target: { value: 'original' }
		});
		expect(screen.queryByRole('radio', { name: /Nord/ })).toBeNull();
		expect(screen.getByRole('radio', { name: /Moss/ })).toBeTruthy();
	});

	it('sets and clears a different dark-mode theme', async () => {
		const onpick = open({ prefs: { ...DEFAULTS, theme_dark: 'moss' } });
		await screen.findByRole('radio', { name: /Nord/ });
		const toggle = await screen.findByRole('switch', {
			name: 'Use a different theme in dark mode'
		});
		expect((toggle as HTMLInputElement).checked).toBe(true);
		await fireEvent.change(screen.getByLabelText('Dark mode theme'), { target: { value: 'nord' } });
		expect(onpick).toHaveBeenCalledWith({ theme_dark: 'nord' });
		await fireEvent.click(toggle);
		expect(onpick).toHaveBeenCalledWith({ theme_dark: '' });
	});

	it('disables the cards when the theme is locked', async () => {
		open({ locked: true });
		expect(
			((await screen.findByRole('radio', { name: /Nord/ })) as HTMLInputElement).disabled
		).toBe(true);
	});

	it('marks a custom theme saved with failing contrast', async () => {
		const bad = { ...e('custom-x-light', 'light', 'X', 'original'), custom: true, warn: true };
		open({ custom: () => [bad, { ...bad, id: 'custom-x-dark', mode: 'dark' }] });
		expect(await screen.findByText('Fails contrast')).toBeTruthy();
	});

	it('shows the failure when themes.json will not load', async () => {
		open({
			themes: async () => {
				throw new Error('offline');
			}
		});
		expect(await screen.findByText(/Couldn't load the themes/)).toBeTruthy();
	});
});
