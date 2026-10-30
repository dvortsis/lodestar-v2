import { describe, expect, it, vi } from 'vitest';
import { configure, fireEvent, render, screen } from '@testing-library/svelte';
import ThemeEditor from './ThemeEditor.svelte';
import { HEX, compile, parseImport, slug, START } from './editor';
import type { ThemesFile } from './themes';

configure({ defaultHidden: true });

const file: ThemesFile = { v: 1, keys: [], themes: [] };
const open = (over: Record<string, unknown> = {}) => {
	const write = vi.fn();
	const onsave = vi.fn();
	render(ThemeEditor, { file, custom: [], write, onsave, onclose: vi.fn(), ...over });
	return { write, onsave };
};

describe('editor engine', () => {
	it('compiles Riverstone with the build compiler and passes its gates', () => {
		const c = compile({ ...START, id: 'custom' }, 'light');
		expect(c.failed).toEqual([]);
		expect(c.entry.id).toBe('custom-light');
		expect(c.entry.values.every((v) => HEX.test(v))).toBe(true);
	});

	it('names ids the server accepts, and keeps them unique', () => {
		expect(slug('My  Théme!', [])).toBe('custom-my-th-me');
		expect(slug('Mine', ['custom-mine'])).toBe('custom-mine-2');
		expect(slug('!!!', [])).toBe('custom-theme');
	});

	it('refuses imports that break the schema', () => {
		expect(parseImport('nope').errors).toEqual(['not JSON']);
		expect(parseImport('{}').family).toBeUndefined();
		expect(parseImport(JSON.stringify(START)).family?.id).toBe('riverstone');
	});
});

describe('ThemeEditor', () => {
	it('shows the live report and saves both modes under one id', async () => {
		const { write, onsave } = open();
		expect(await screen.findByRole('table')).toBeTruthy();
		expect(screen.getByText(/All checks pass/)).toBeTruthy();
		await fireEvent.input(screen.getByLabelText('Theme name'), { target: { value: 'My Theme' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Save theme' }));
		expect(onsave).toHaveBeenCalledWith('custom-my-theme');
		const saved = write.mock.calls[0]![0] as { id: string; warn?: boolean }[];
		expect(saved.map((t) => t.id)).toEqual(['custom-my-theme-light', 'custom-my-theme-dark']);
		expect(saved.some((t) => t.warn)).toBe(false);
	});

	it('needs "Use anyway" for a failing theme and marks it', async () => {
		const { write, onsave } = open();
		await fireEvent.input(screen.getByLabelText('Theme name'), { target: { value: 'Faint' } });
		// Text the colour of the background fails the text gate.
		await fireEvent.input(screen.getByLabelText('Text, hex value'), {
			target: { value: START.light.background }
		});
		expect(screen.getByText(/checks? fail/)).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Save theme' }));
		expect(onsave).not.toHaveBeenCalled();
		await fireEvent.click(screen.getByRole('button', { name: 'Use anyway' }));
		expect(onsave).toHaveBeenCalled();
		expect((write.mock.calls[0]![0] as { warn?: boolean }[]).every((t) => t.warn)).toBe(true);
	});

	it('asks for valid hex before it reports', async () => {
		open();
		await fireEvent.input(screen.getByLabelText('Primary, hex value'), {
			target: { value: '#12' }
		});
		expect(await screen.findByText(/six-digit hex/)).toBeTruthy();
	});

	it('exports and re-imports a theme', async () => {
		open();
		await fireEvent.input(screen.getByLabelText('Theme name'), { target: { value: 'Round' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Export JSON' }));
		const box = screen.getByLabelText('Import or export') as HTMLTextAreaElement;
		expect(JSON.parse(box.value).name).toBe('Round');
		await fireEvent.input(screen.getByLabelText('Theme name'), { target: { value: 'Other' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Import JSON' }));
		expect((screen.getByLabelText('Theme name') as HTMLInputElement).value).toBe('Round');
	});
});
