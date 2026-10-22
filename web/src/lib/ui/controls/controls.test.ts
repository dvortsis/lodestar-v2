import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { text } from '../testing';
import Button from './Button.svelte';
import IconButton from './IconButton.svelte';
import SegmentedControl from './SegmentedControl.svelte';

describe('Button', () => {
	it('renders each variant and density', () => {
		for (const variant of ['primary', 'secondary', 'quiet', 'danger'] as const) {
			const { unmount } = render(Button, { variant, dense: true, children: text(variant) });
			const button = screen.getByRole('button', { name: variant });
			expect(button.className).toContain('btn-sm');
			unmount();
		}
	});

	it('defaults to type=button and fires onclick', async () => {
		const onclick = vi.fn();
		render(Button, { onclick, children: text('Save') });
		const button = screen.getByRole('button', { name: 'Save' });
		expect(button.getAttribute('type')).toBe('button');
		await fireEvent.click(button);
		expect(onclick).toHaveBeenCalledOnce();
	});

	it('loading keeps the label (width), sets aria-busy and ignores clicks, staying focusable', async () => {
		const onclick = vi.fn();
		render(Button, { onclick, loading: true, children: text('Save') });
		const button = screen.getByRole('button', { name: 'Save' });
		expect(button.getAttribute('aria-busy')).toBe('true');
		expect(button.getAttribute('aria-disabled')).toBe('true');
		expect((button as HTMLButtonElement).disabled).toBe(false);
		button.focus();
		expect(document.activeElement).toBe(button);
		await fireEvent.click(button);
		expect(onclick).not.toHaveBeenCalled();
	});

	it('disabled uses the native attribute (browsers then suppress clicks and focus)', () => {
		render(Button, { disabled: true, children: text('Save') });
		const button = screen.getByRole('button', { name: 'Save' });
		expect((button as HTMLButtonElement).disabled).toBe(true);
		expect(button.getAttribute('aria-disabled')).toBeNull();
	});
});

describe('IconButton', () => {
	it('is named by its label and hides the icon', () => {
		render(IconButton, { label: 'Close', children: text('x') });
		const button = screen.getByRole('button', { name: 'Close' });
		expect(button.querySelector('[aria-hidden="true"]')).not.toBeNull();
	});
});

describe('SegmentedControl', () => {
	const options = [
		{ value: 'a', label: 'Alpha' },
		{ value: 'b', label: 'Beta', disabled: true },
		{ value: 'c', label: 'Gamma' }
	];

	it('has radiogroup semantics with one tab stop on the checked radio', () => {
		render(SegmentedControl, { options, value: 'c', label: 'Greek' });
		expect(screen.getByRole('radiogroup', { name: 'Greek' })).toBeTruthy();
		const radios = screen.getAllByRole('radio');
		expect(radios.map((r) => r.getAttribute('aria-checked'))).toEqual(['false', 'false', 'true']);
		expect(radios.map((r) => r.getAttribute('tabindex'))).toEqual(['-1', '-1', '0']);
	});

	it('arrow keys move and select, skipping disabled options and wrapping', async () => {
		const onchange = vi.fn();
		render(SegmentedControl, { options, value: 'a', label: 'Greek', onchange });
		const alpha = screen.getByRole('radio', { name: 'Alpha' });
		alpha.focus();
		await fireEvent.keyDown(alpha, { key: 'ArrowRight' });
		expect(onchange).toHaveBeenLastCalledWith('c');
		expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'Gamma' }));
		await fireEvent.keyDown(screen.getByRole('radio', { name: 'Gamma' }), { key: 'ArrowRight' });
		expect(onchange).toHaveBeenLastCalledWith('a');
		await fireEvent.keyDown(screen.getByRole('radio', { name: 'Alpha' }), { key: 'End' });
		expect(onchange).toHaveBeenLastCalledWith('c');
		await fireEvent.keyDown(screen.getByRole('radio', { name: 'Gamma' }), { key: 'ArrowLeft' });
		expect(onchange).toHaveBeenLastCalledWith('a');
	});

	it('click selects; disabled options do not', async () => {
		const onchange = vi.fn();
		render(SegmentedControl, { options, value: 'a', label: 'Greek', onchange });
		await fireEvent.click(screen.getByRole('radio', { name: 'Beta' }));
		expect(onchange).not.toHaveBeenCalled();
		await fireEvent.click(screen.getByRole('radio', { name: 'Gamma' }));
		expect(onchange).toHaveBeenCalledWith('c');
	});
});
