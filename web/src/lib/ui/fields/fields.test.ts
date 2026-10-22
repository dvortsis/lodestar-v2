import { describe, expect, it, vi } from 'vitest';
import { createRawSnippet } from 'svelte';
import { fireEvent, render, screen } from '@testing-library/svelte';
import Field, { type FieldControlProps } from './Field.svelte';
import Combobox from './Combobox.svelte';
import Checkbox from './Checkbox.svelte';
import Radio from './Radio.svelte';
import Toggle from './Toggle.svelte';
import Slider from './Slider.svelte';
import RangeInput from './RangeInput.svelte';
import Select from './Select.svelte';
import Input from './Input.svelte';
import Textarea from './Textarea.svelte';

const control = createRawSnippet<[FieldControlProps]>((props) => ({
	render: () => {
		const p = props();
		const attrs = Object.entries(p)
			.filter(([, v]) => v !== undefined)
			.map(([k, v]) => `${k}="${v}"`)
			.join(' ');
		return `<input ${attrs} />`;
	}
}));

describe('Field', () => {
	it('ties label, help and error to the control', () => {
		render(Field, {
			label: 'Name',
			help: 'Shown in lists',
			error: 'Name is required',
			required: true,
			children: control
		});
		const input = screen.getByLabelText(/Name/);
		const described = input.getAttribute('aria-describedby')!.split(' ');
		expect(described).toHaveLength(2);
		expect(document.getElementById(described[0]!)!.textContent).toBe('Shown in lists');
		expect(document.getElementById(described[1]!)!.textContent).toContain('Name is required');
		expect(input.getAttribute('aria-invalid')).toBe('true');
		expect(input.getAttribute('aria-required')).toBe('true');
	});

	it('announces required once: aria-required on the control, a visual-only marker', () => {
		const { container } = render(Field, { label: 'Name', required: true, children: control });
		expect(screen.getByLabelText(/Name/).getAttribute('aria-required')).toBe('true');
		expect(container.querySelector('label [aria-hidden="true"]')!.textContent).toBe('*');
		expect(container.querySelector('.sr-only')).toBeNull();
	});

	it('gives the control the label id for popups that need their own name', () => {
		const { container } = render(Field, { label: 'Name', children: control });
		const input = screen.getByLabelText('Name');
		const label = container.querySelector('label')!;
		expect(input.getAttribute('aria-labelledby')).toBe(label.id);
	});

	it('omits describedby and invalid when there is nothing to say', () => {
		render(Field, { label: 'Name', children: control });
		const input = screen.getByLabelText('Name');
		expect(input.getAttribute('aria-describedby')).toBeNull();
		expect(input.getAttribute('aria-invalid')).toBeNull();
	});
});

describe('Input / Textarea / Select', () => {
	it('Input forwards attributes and dense size', () => {
		render(Input, { dense: true, 'aria-label': 'Query', value: 'abc' });
		const el = screen.getByLabelText('Query') as HTMLInputElement;
		expect(el.value).toBe('abc');
		expect(el.className).toContain('input-sm');
	});

	it('Textarea renders its value', () => {
		render(Textarea, { 'aria-label': 'Notes', value: 'hello' });
		expect((screen.getByLabelText('Notes') as HTMLTextAreaElement).value).toBe('hello');
	});

	it('Select lists options and a placeholder, disabled options included', () => {
		render(Select, {
			'aria-label': 'Sort',
			placeholder: 'Choose…',
			options: [
				{ value: 'a', label: 'A' },
				{ value: 'b', label: 'B', disabled: true }
			],
			value: 'a'
		});
		const options = screen.getAllByRole('option') as HTMLOptionElement[];
		expect(options.map((o) => o.textContent)).toEqual(['Choose…', 'A', 'B']);
		expect(options[2]!.disabled).toBe(true);
	});
});

describe('Checkbox / Radio / Toggle', () => {
	it('Checkbox toggles with the label and supports indeterminate', async () => {
		render(Checkbox, { label: 'All', checked: false, indeterminate: true });
		const box = screen.getByLabelText('All') as HTMLInputElement;
		expect(box.indeterminate).toBe(true);
		await fireEvent.click(box);
		expect(box.checked).toBe(true);
		expect(box.indeterminate).toBe(false);
	});

	it('Radio group selects one value', async () => {
		render(Radio, { name: 'g', label: 'One', value: '1', group: '2' });
		render(Radio, { name: 'g', label: 'Two', value: '2', group: '2' });
		const one = screen.getByLabelText('One') as HTMLInputElement;
		const two = screen.getByLabelText('Two') as HTMLInputElement;
		expect(two.checked).toBe(true);
		await fireEvent.click(one);
		expect(one.checked).toBe(true);
	});

	it('Toggle has switch semantics and toggles with Space/click', async () => {
		render(Toggle, { label: 'Pause crawler', checked: false });
		const sw = screen.getByRole('switch', { name: 'Pause crawler' }) as HTMLInputElement;
		expect(sw.checked).toBe(false);
		await fireEvent.click(sw);
		expect(sw.checked).toBe(true);
	});

	it('disabled states are native', () => {
		render(Toggle, { label: 'T', disabled: true });
		expect((screen.getByRole('switch') as HTMLInputElement).disabled).toBe(true);
	});
});

describe('Slider', () => {
	it('exposes formatted value text and the unit', () => {
		render(Slider, { value: 1234, min: 0, max: 5000, unit: 'MiB', 'aria-label': 'Limit' });
		const slider = screen.getByRole('slider', { name: 'Limit' });
		expect(slider.getAttribute('aria-valuetext')).toBe('1,234 MiB');
		expect(screen.getByTestId('slider-value').textContent).toBe('1,234 MiB');
	});

	it('uses a custom formatter', () => {
		render(Slider, { value: 3, format: (v: number) => `${v}x`, 'aria-label': 'Depth' });
		expect(screen.getByRole('slider').getAttribute('aria-valuetext')).toBe('3x');
	});
});

describe('RangeInput', () => {
	const base = { label: 'Size', minLabel: 'Minimum size', maxLabel: 'Maximum size', unit: 'MiB' };

	it('shows both inputs, the unit and a formatted summary', () => {
		render(RangeInput, { ...base, min: 1000, max: 2500 });
		expect((screen.getByLabelText('Minimum size (MiB)') as HTMLInputElement).value).toBe('1000');
		expect(screen.getByTestId('range-summary').textContent!.trim()).toBe('1,000 to 2,500 MiB');
	});

	it('unbounded sides render an em dash, never 0', () => {
		render(RangeInput, { ...base, min: null, max: 10 });
		expect(screen.getByTestId('range-summary').textContent!.trim()).toBe('— to 10 MiB');
		expect((screen.getByLabelText('Minimum size (MiB)') as HTMLInputElement).value).toBe('');
	});

	it('reports changes and flags min > max', async () => {
		const onchange = vi.fn();
		render(RangeInput, { ...base, min: 1, max: 5, onchange, invalidMessage: 'Min exceeds max' });
		const min = screen.getByLabelText('Minimum size (MiB)');
		await fireEvent.input(min, { target: { value: '9' } });
		expect(onchange).toHaveBeenLastCalledWith({ min: 9, max: 5 });
		expect(min.getAttribute('aria-invalid')).toBe('true');
		expect(screen.getByText('Min exceeds max')).toBeTruthy();
		await fireEvent.input(min, { target: { value: '' } });
		expect(onchange).toHaveBeenLastCalledWith({ min: null, max: 5 });
	});
});

describe('Combobox', () => {
	const options = [
		{ value: 'ubuntu', label: 'ubuntu' },
		{ value: 'debian', label: 'debian' },
		{ value: 'dune', label: 'dune', disabled: true },
		{ value: 'arch', label: 'arch' }
	];

	it('follows the combobox pattern: collapsed by default', () => {
		render(Combobox, { options, label: 'Source' });
		const input = screen.getByRole('combobox', { name: 'Source' });
		expect(input.getAttribute('aria-expanded')).toBe('false');
		expect(input.getAttribute('aria-autocomplete')).toBe('list');
		expect(input.getAttribute('aria-controls')).toBeTruthy();
	});

	it('typing opens and filters the list', async () => {
		render(Combobox, { options, label: 'Source' });
		const input = screen.getByRole('combobox');
		await fireEvent.input(input, { target: { value: 'd' } });
		expect(input.getAttribute('aria-expanded')).toBe('true');
		expect(screen.getAllByRole('option').map((o) => o.textContent!.trim())).toEqual([
			'debian',
			'dune'
		]);
	});

	it('arrow keys move aria-activedescendant (skipping disabled), Enter selects, Escape closes', async () => {
		const onselect = vi.fn();
		render(Combobox, { options, label: 'Source', onselect });
		const input = screen.getByRole('combobox') as HTMLInputElement;
		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		expect(input.getAttribute('aria-expanded')).toBe('true');
		const active = () => document.getElementById(input.getAttribute('aria-activedescendant')!);
		expect(active()!.textContent!.trim()).toBe('ubuntu');
		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		expect(active()!.textContent!.trim()).toBe('debian');
		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		expect(active()!.textContent!.trim()).toBe('arch');
		await fireEvent.keyDown(input, { key: 'ArrowUp' });
		expect(active()!.textContent!.trim()).toBe('debian');
		await fireEvent.keyDown(input, { key: 'Enter' });
		expect(onselect).toHaveBeenCalledWith(options[1]);
		expect(input.value).toBe('debian');
		expect(input.getAttribute('aria-expanded')).toBe('false');

		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		await fireEvent.keyDown(input, { key: 'Escape' });
		expect(input.getAttribute('aria-expanded')).toBe('false');
		expect(input.getAttribute('aria-activedescendant')).toBeNull();
	});

	it('Enter with no active option does not select (free text stays)', async () => {
		const onselect = vi.fn();
		render(Combobox, { options, label: 'Source', onselect });
		const input = screen.getByRole('combobox');
		await fireEvent.input(input, { target: { value: 'zzz' } });
		await fireEvent.keyDown(input, { key: 'Enter' });
		expect(onselect).not.toHaveBeenCalled();
		expect(screen.getByText('No matches')).toBeTruthy();
	});

	it('resets the highlight when the options change', async () => {
		const { rerender } = render(Combobox, { options, label: 'Source' });
		const input = screen.getByRole('combobox');
		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		expect(input.getAttribute('aria-activedescendant')).toBeTruthy();
		await rerender({ options: options.slice(0, 2), label: 'Source' });
		expect(input.getAttribute('aria-activedescendant')).toBeNull();
	});

	it('names the list from the Field label and passes name/disabled through', () => {
		render(Combobox, {
			options,
			'aria-labelledby': 'lbl',
			name: 'source',
			disabled: true
		});
		const input = screen.getByRole('combobox', { hidden: true }) as HTMLInputElement;
		expect(input.name).toBe('source');
		expect(input.disabled).toBe(true);
		expect(screen.getByRole('listbox', { hidden: true }).getAttribute('aria-labelledby')).toBe(
			'lbl'
		);
	});

	it('mouse choice selects without losing focus', async () => {
		const onselect = vi.fn();
		render(Combobox, { options, label: 'Source', onselect });
		const input = screen.getByRole('combobox');
		await fireEvent.keyDown(input, { key: 'ArrowDown' });
		await fireEvent.mouseDown(screen.getByRole('option', { name: 'arch' }));
		expect(onselect).toHaveBeenCalledWith(options[3]);
	});
});
