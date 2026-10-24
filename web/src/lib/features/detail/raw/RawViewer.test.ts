import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import RawViewer from './RawViewer.svelte';

const sample = {
	name: 'x',
	'piece length': 16384,
	files: [{ length: 1, path: ['a', 'b.txt'] }],
	nothing: null,
	ok: true
};
const toggle = (name: string) =>
	screen.getByRole('button', { name: new RegExp(`^${name} [\\[{]\\d+[\\]}]$`) });

describe('RawViewer', () => {
	it('opens at the root and discloses nested values with the keyboard-reachable buttons', async () => {
		render(RawViewer, { value: sample, label: 'Info' });
		const root = screen.getByTestId('detail-raw');
		expect(root.textContent).toContain('name: "x"');
		expect(root.textContent).toContain('piece length: 16384');
		expect(root.textContent).toContain('nothing: null');
		expect(root.textContent).toContain('ok: true');
		// nested values start closed
		expect(root.textContent).not.toContain('b.txt');
		const files = toggle('files');
		expect(files.getAttribute('aria-expanded')).toBe('false');
		await fireEvent.click(files);
		expect(files.getAttribute('aria-expanded')).toBe('true');
		expect(files.textContent).toContain('[1]');
		await fireEvent.click(toggle('0'));
		await fireEvent.click(toggle('path'));
		expect(root.textContent).toContain('1: "b.txt"');
		await fireEvent.click(toggle('files'));
		expect(root.textContent).not.toContain('b.txt');
	});

	it('shows a string that looks like markup as text and creates no elements from it', () => {
		render(RawViewer, { value: { note: '<img src=x onerror=alert(1)><script>alert(2)</script>' } });
		const root = screen.getByTestId('detail-raw');
		expect(root.textContent).toContain('<img src=x onerror=alert(1)><script>alert(2)</script>');
		expect(root.querySelector('img, script')).toBeNull();
	});

	it('cuts long strings with a count and reveals them on request', async () => {
		const long = 'x'.repeat(500);
		render(RawViewer, { value: { long }, maxString: 40 });
		const root = screen.getByTestId('detail-raw');
		expect(root.textContent).not.toContain(long);
		expect(root.textContent).toContain('500 characters, cut');
		await fireEvent.click(screen.getByRole('button', { name: 'Show all' }));
		expect(root.textContent).toContain(long);
		expect(screen.queryByRole('button', { name: 'Show all' })).toBeNull();
	});

	it('limits depth', async () => {
		render(RawViewer, { value: { a: { b: { c: 1 } } }, maxDepth: 2 });
		await fireEvent.click(toggle('a'));
		await fireEvent.click(toggle('b'));
		const root = screen.getByTestId('detail-raw');
		expect(root.textContent).toContain('Too deep to show');
		expect(root.textContent).not.toContain('c: 1');
	});

	it('lists long arrays in part with a way to see the rest', async () => {
		render(RawViewer, {
			value: { items: Array.from({ length: 250 }, (_, i) => i) },
			maxItems: 100
		});
		await fireEvent.click(toggle('items'));
		const root = screen.getByTestId('detail-raw');
		expect(root.textContent).toContain('99: 99');
		expect(root.textContent).not.toContain('100: 100');
		await fireEvent.click(screen.getByRole('button', { name: 'Show all (250)' }));
		expect(root.textContent).toContain('249: 249');
	});

	it('renders scalars and empty containers', () => {
		render(RawViewer, { value: 42 });
		expect(screen.getByTestId('detail-raw').textContent).toContain('42');
	});

	it('shows bidirectional controls in keys and strings as markers', () => {
		render(RawViewer, { value: { 'a\u202Eb': 'x\u202Ey' } });
		const root = screen.getByTestId('detail-raw');
		expect(root.textContent).toContain('a<U+202E>b: "x<U+202E>y"');
		expect(root.textContent).not.toContain('\u202E');
	});

	it('does not confuse a key with a dot with a nested key', async () => {
		render(RawViewer, { value: { 'a.b': { x: 1 }, a: { b: { x: 2 } } } });
		await fireEvent.click(screen.getByRole('button', { name: /^a\.b \{1\}$/ }));
		const root = screen.getByTestId('detail-raw');
		expect(root.textContent).toContain('x: 1');
		expect(root.textContent).not.toContain('x: 2');
	});
});
