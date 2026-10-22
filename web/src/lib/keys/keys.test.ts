import { describe, expect, it, vi } from 'vitest';
import { comboFromEvent, normalizeCombo } from './combo';
import { formatCombo } from './display';
import { filterCommands } from './filter';
import { createKeys, type Conflict } from './registry.svelte';

const ev = (key: string, init: KeyboardEventInit = {}, target?: Element) => {
	const e = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
	if (target) Object.defineProperty(e, 'target', { value: target });
	return e;
};
const el = <K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}) => {
	const node = document.createElement(tag);
	for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
	document.body.append(node);
	return node;
};
const make = () => {
	const conflicts: Conflict[] = [];
	return { keys: createKeys({ mac: false, onConflict: (c) => conflicts.push(c) }), conflicts };
};

describe('combos', () => {
	it('normalizes specs and events to one form', () => {
		expect(normalizeCombo('Mod+K', false)).toBe('ctrl+k');
		expect(normalizeCombo('mod+k', true)).toBe('meta+k');
		expect(normalizeCombo('shift+ctrl+P')).toBe('ctrl+shift+p');
		expect(normalizeCombo('Esc')).toBe('escape');
		expect(normalizeCombo('?')).toBe('?');
		expect(normalizeCombo('shift+?')).toBe('?');
		expect(comboFromEvent(ev('?', { shiftKey: true }))).toBe('?');
		expect(comboFromEvent(ev('K', { ctrlKey: true, shiftKey: true }))).toBe('ctrl+shift+k');
		expect(comboFromEvent(ev('Tab', { shiftKey: true }))).toBe('shift+tab');
		expect(() => normalizeCombo('hyper+k')).toThrow(/unknown modifier/);
		expect(() => normalizeCombo('')).toThrow(/empty/);
	});

	it('formats for display', () => {
		expect(formatCombo('mod+k', true)).toBe('⌘K');
		expect(formatCombo('mod+k', false)).toBe('Ctrl+K');
		expect(formatCombo('Escape', false)).toBe('Esc');
		expect(formatCombo('/', false)).toBe('/');
	});
});

describe('registry', () => {
	it('runs a command by default key, custom key and unregisters', () => {
		const { keys } = make();
		const focus = vi.fn();
		const open = vi.fn();
		const off = keys.register({ id: 'search.focus', title: 'Focus search', run: focus });
		keys.register({ id: 'custom', title: 'Custom', keys: ['g'], run: open });
		expect(keys.handle(ev('/'))).toBe(true);
		expect(keys.handle(ev('g'))).toBe(true);
		expect(focus).toHaveBeenCalledOnce();
		expect(open).toHaveBeenCalledOnce();
		off();
		expect(keys.handle(ev('/'))).toBe(false);
		expect(keys.list().map((c) => c.id)).toEqual(['custom']);
	});

	it('prevents default only when a command ran', () => {
		const { keys } = make();
		keys.register({ id: 'list.next', title: 'Next', run: () => {} });
		const hit = ev('j');
		const miss = ev('q');
		keys.handle(hit);
		keys.handle(miss);
		expect(hit.defaultPrevented).toBe(true);
		expect(miss.defaultPrevented).toBe(false);
	});

	it('rejects a duplicate id and a malformed combo at registration', () => {
		const { keys } = make();
		keys.register({ id: 'a', title: 'A', keys: ['a'], run: () => {} });
		expect(() => keys.register({ id: 'a', title: 'A', run: () => {} })).toThrow(/already/);
		expect(() => keys.register({ id: 'b', title: 'B', keys: ['nope+b'], run: () => {} })).toThrow();
	});

	it('reports a conflict and fires neither command', () => {
		const { keys, conflicts } = make();
		const a = vi.fn();
		const b = vi.fn();
		keys.register({ id: 'a', title: 'A', keys: ['x'], run: a });
		keys.register({ id: 'b', title: 'B', keys: ['x'], run: b });
		expect(conflicts).toEqual([{ combo: 'x', scope: 'global', ids: ['a', 'b'] }]);
		expect(keys.conflicts()).toHaveLength(1);
		expect(keys.handle(ev('x'))).toBe(false);
		expect(a).not.toHaveBeenCalled();
		expect(b).not.toHaveBeenCalled();
	});

	it('does not call the same key in different scopes a conflict', () => {
		const { keys, conflicts } = make();
		keys.register({ id: 'a', title: 'A', keys: ['x'], run: () => {} });
		keys.register({ id: 'b', title: 'B', keys: ['x'], scope: 'search', run: () => {} });
		expect(conflicts).toEqual([]);
	});

	it('scopes: a pushed scope wins, falls back to global, and pops', () => {
		const { keys } = make();
		const g = vi.fn();
		const s = vi.fn();
		const other = vi.fn();
		keys.register({ id: 'g', title: 'G', keys: ['x'], run: g });
		keys.register({ id: 's', title: 'S', keys: ['x'], scope: 'search', run: s });
		keys.register({ id: 'o', title: 'O', keys: ['y'], scope: 'search', run: other });
		expect(keys.handle(ev('y'))).toBe(false); // scope not active
		keys.handle(ev('x'));
		expect(g).toHaveBeenCalledTimes(1);
		const pop = keys.pushScope('search');
		keys.handle(ev('x'));
		expect(s).toHaveBeenCalledTimes(1);
		expect(g).toHaveBeenCalledTimes(1);
		expect(keys.handle(ev('y'))).toBe(true);
		pop();
		keys.handle(ev('x'));
		expect(g).toHaveBeenCalledTimes(2);
	});

	it('an exclusive scope mutes the commands below it', () => {
		const { keys } = make();
		const g = vi.fn();
		keys.register({ id: 'g', title: 'G', keys: ['x'], run: g });
		keys.register({ id: 'm', title: 'M', keys: ['z'], scope: 'modal', run: () => {} });
		const pop = keys.pushScope('modal', { exclusive: true });
		expect(keys.handle(ev('x'))).toBe(false);
		expect(keys.handle(ev('z'))).toBe(true);
		pop();
		expect(keys.handle(ev('x'))).toBe(true);
	});

	it('never fires a single-key shortcut while typing', () => {
		const { keys } = make();
		const run = vi.fn();
		keys.register({ id: 'list.next', title: 'Next', allowInInput: true, run });
		for (const target of [
			el('input', { type: 'text' }),
			el('input'),
			el('input', { type: 'search' }),
			el('textarea'),
			el('select'),
			el('div', { role: 'textbox' }),
			el('div', { role: 'combobox' })
		]) {
			expect(keys.handle(ev('j', {}, target))).toBe(false);
		}
		const editable = el('div');
		Object.defineProperty(editable, 'isContentEditable', { value: true });
		expect(keys.handle(ev('j', {}, editable))).toBe(false);
		expect(run).not.toHaveBeenCalled();
		// Not a text field: the key works.
		expect(keys.handle(ev('j', {}, el('input', { type: 'checkbox' })))).toBe(true);
		expect(keys.handle(ev('j', {}, el('button')))).toBe(true);
	});

	it('allows modifier combos and Escape in a field only when the command opts in', () => {
		const { keys } = make();
		const palette = vi.fn();
		const clear = vi.fn();
		const plain = vi.fn();
		keys.register({ id: 'palette.open', title: 'Palette', allowInInput: true, run: palette });
		keys.register({ id: 'dismiss', title: 'Clear', allowInInput: true, run: clear });
		keys.register({ id: 'p', title: 'Plain', keys: ['mod+j'], run: plain });
		const input = el('input', { type: 'text' });
		expect(keys.handle(ev('k', { ctrlKey: true }, input))).toBe(true);
		expect(keys.handle(ev('Escape', {}, input))).toBe(true);
		expect(keys.handle(ev('j', { ctrlKey: true }, input))).toBe(false);
		expect(palette).toHaveBeenCalledOnce();
		expect(clear).toHaveBeenCalledOnce();
		expect(plain).not.toHaveBeenCalled();
	});

	it('ignores composing and already-handled events, and single keys inside dialogs', () => {
		const { keys } = make();
		const run = vi.fn();
		keys.register({ id: 'list.next', title: 'Next', run });
		keys.register({ id: 'palette.open', title: 'P', run: () => {} });
		expect(keys.handle(ev('j', { isComposing: true }))).toBe(false);
		const handled = ev('j');
		handled.preventDefault();
		expect(keys.handle(handled)).toBe(false);
		const dialog = el('div', { role: 'dialog' });
		const inside = dialog.appendChild(document.createElement('button'));
		expect(keys.handle(ev('j', {}, inside))).toBe(false);
		expect(keys.handle(ev('k', { ctrlKey: true }, inside))).toBe(true); // modifier combos still work
		expect(run).not.toHaveBeenCalled();
	});

	it('turns single-key shortcuts off, and keeps modifier combos', () => {
		const { keys } = make();
		keys.register({ id: 'list.next', title: 'Next', run: () => {} });
		keys.register({ id: 'palette.open', title: 'P', run: () => {} });
		keys.setSingleKeys(false);
		expect(keys.singleKeys).toBe(false);
		expect(keys.handle(ev('j'))).toBe(false);
		expect(keys.handle(ev('k', { ctrlKey: true }))).toBe(true);
		keys.setSingleKeys(true);
		expect(keys.handle(ev('j'))).toBe(true);
	});

	it('remaps a command and restores its default', () => {
		const { keys } = make();
		const run = vi.fn();
		keys.register({ id: 'magnet.copy', title: 'Copy magnet', run });
		keys.remap('magnet.copy', ['y']);
		expect(keys.handle(ev('c'))).toBe(false);
		expect(keys.handle(ev('y'))).toBe(true);
		expect(keys.list()[0]!.keys).toEqual(['y']);
		keys.remap('magnet.copy', null);
		expect(keys.handle(ev('c'))).toBe(true);
	});

	it('honours when() and exposes titles, sections and keys for the palette and sheet', () => {
		const { keys } = make();
		let on = false;
		keys.register({
			id: 'nav.alerts',
			title: () => 'Go to Alerts',
			section: 'Navigate',
			keywords: ['saved'],
			when: () => on,
			run: () => {}
		});
		keys.register({ id: 'row.select', title: 'Select row', palette: false, run: () => {} });
		const [a, b] = keys.list();
		expect(a).toMatchObject({
			title: 'Go to Alerts',
			section: 'Navigate',
			active: false,
			palette: true
		});
		expect(b).toMatchObject({ keys: ['x'], palette: false });
		expect(keys.run('nav.alerts')).toBe(false);
		on = true;
		expect(keys.run('nav.alerts')).toBe(true);
		expect(keys.run('missing')).toBe(false);
	});

	it('attach listens on the target and detaches', () => {
		const { keys } = make();
		const run = vi.fn();
		keys.register({ id: 'list.next', title: 'Next', run });
		const detach = keys.attach(document);
		document.body.dispatchEvent(ev('j'));
		detach();
		document.body.dispatchEvent(ev('j'));
		expect(run).toHaveBeenCalledOnce();
	});
});

describe('filterCommands', () => {
	const items = [
		{ title: 'Go to Search' },
		{ title: 'Go to Alerts', keywords: ['saved searches'] },
		{ title: 'Copy magnet', section: 'Torrent' },
		{ title: 'Open Insights' },
		{ title: 'Switch to dark theme', keywords: ['appearance'] }
	];
	const titles = (q: string) => filterCommands(items, q).map((i) => i.title);

	it('returns everything in order for an empty query', () => {
		expect(titles('')).toEqual(items.map((i) => i.title));
		expect(titles('   ')).toHaveLength(5);
	});

	it('is case-insensitive and needs every word', () => {
		expect(titles('GO ALERTS')).toEqual(['Go to Alerts']);
		expect(titles('go zzz')).toEqual([]);
	});

	it('ranks prefix, then word start, then substring, then keywords; ties keep order', () => {
		// "Switch" starts with s; "Search" and "Insights" contain a word/substring; Alerts only via keyword.
		expect(titles('s')).toEqual([
			'Switch to dark theme',
			'Go to Search',
			'Go to Alerts',
			'Open Insights'
		]);
		expect(titles('ins')[0]).toBe('Open Insights');
		expect(titles('saved')).toEqual(['Go to Alerts']);
		expect(titles('torrent')).toEqual(['Copy magnet']);
		expect(titles('appearance')).toEqual(['Switch to dark theme']);
	});
});
