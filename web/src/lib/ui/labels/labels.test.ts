import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { flushSync, mount, unmount } from 'svelte';
import { text } from '../testing';
import Badge from './Badge.svelte';
import Chip from './Chip.svelte';
import Kbd from './Kbd.svelte';
import Mono from './Mono.svelte';
import RelativeTime from './RelativeTime.svelte';
import { tickerSubscribers } from './ticker.svelte';

describe('Badge', () => {
	it('status variants carry an icon as well as text', () => {
		for (const variant of ['info', 'success', 'warning', 'error'] as const) {
			const { container, unmount } = render(Badge, { variant, children: text(variant) });
			expect(container.querySelector('svg[aria-hidden="true"]')).not.toBeNull();
			expect(container.textContent).toContain(variant);
			unmount();
		}
	});

	it('neutral and accent are plain labels', () => {
		const { container } = render(Badge, { variant: 'neutral', children: text('Beta') });
		expect(container.querySelector('svg')).toBeNull();
	});
});

describe('Chip', () => {
	it('has no remove button unless removable', () => {
		render(Chip, { children: text('video') });
		expect(screen.queryByRole('button')).toBeNull();
	});

	it('the remove button has an accessible name and removes on click', async () => {
		const onremove = vi.fn();
		render(Chip, { children: text('video'), onremove, removeLabel: 'Remove filter: video' });
		await fireEvent.click(screen.getByRole('button', { name: 'Remove filter: video' }));
		expect(onremove).toHaveBeenCalledOnce();
	});

	it('Backspace and Delete remove when the remove button is focused; other keys do not', async () => {
		const onremove = vi.fn();
		render(Chip, { children: text('video'), onremove, removeLabel: 'Remove' });
		const button = screen.getByRole('button', { name: 'Remove' });
		button.focus();
		await fireEvent.keyDown(button, { key: 'Backspace' });
		await fireEvent.keyDown(button, { key: 'Delete' });
		await fireEvent.keyDown(button, { key: 'a' });
		expect(onremove).toHaveBeenCalledTimes(2);
	});

	it('returns focus to the next chip, then the previous, then the fallback', async () => {
		const host = document.createElement('div');
		document.body.append(host);
		const fallback = document.createElement('input');
		document.body.append(fallback);
		const names = ['a', 'b', 'c'];
		const live = new Set(names);
		const mounted = names.map((n) => {
			return mount(Chip, {
				target: host,
				props: {
					children: text(n),
					removeLabel: `Remove ${n}`,
					fallbackFocus: () => fallback,
					onremove: () => {
						live.delete(n);
						unmount(mounted[names.indexOf(n)]!);
					}
				}
			});
		});
		flushSync(); // mount() leaves bind:this effects pending
		const press = async (n: string) => {
			const b = host.querySelector(`[aria-label="Remove ${n}"]`) as HTMLElement;
			b.focus();
			await fireEvent.keyDown(b, { key: 'Delete' });
			await new Promise((r) => setTimeout(r, 0));
		};
		await press('b');
		expect(document.activeElement!.getAttribute('aria-label')).toBe('Remove c');
		await press('c');
		expect(document.activeElement!.getAttribute('aria-label')).toBe('Remove a');
		await press('a');
		expect(document.activeElement).toBe(fallback);
		host.remove();
		fallback.remove();
	});

	it('falls back to the shared remove label', () => {
		render(Chip, { children: text('video'), onremove: () => {} });
		expect(screen.getByRole('button', { name: 'Remove' })).toBeTruthy();
	});
});

describe('Kbd / Mono', () => {
	it('render semantic elements', () => {
		const { container } = render(Kbd, { children: text('/') });
		expect(container.querySelector('kbd')).not.toBeNull();
		const mono = render(Mono, { children: text('3b24'), wrap: true });
		const code = mono.container.querySelector('code')!;
		expect(code.className).toContain('font-mono');
		expect(code.className).toContain('break-all');
	});
});

describe('RelativeTime', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date('2026-10-22T12:00:00Z'));
	});
	afterEach(() => vi.useRealTimers());

	const NOW = Date.parse('2026-10-22T12:00:00Z') / 1000;

	it('shows relative text, the absolute time as title and accessible text', () => {
		const { container } = render(RelativeTime, { seconds: NOW - 3 * 3600, timeZone: 'UTC' });
		const time = container.querySelector('time')!;
		expect(time.textContent).toContain('3 hours ago');
		expect(time.getAttribute('title')).toBe('Oct 22, 2026, 09:00');
		expect(time.querySelector('.sr-only')!.textContent).toBe('(Oct 22, 2026, 09:00)');
		expect(time.getAttribute('datetime')).toBe('2026-10-22T09:00:00.000Z');
	});

	it('null renders an em dash with no datetime', () => {
		const { container } = render(RelativeTime, { seconds: null });
		const time = container.querySelector('time')!;
		expect(time.textContent!.trim()).toBe('—');
		expect(time.getAttribute('datetime')).toBeNull();
		expect(time.getAttribute('title')).toBeNull();
	});

	it('uses the shared just-now message', () => {
		const { container } = render(RelativeTime, { seconds: NOW - 5 });
		expect(container.querySelector('time')!.textContent).toContain('just now');
	});

	it('many instances share one ticker, and it refreshes the text', () => {
		const spy = vi.spyOn(globalThis, 'setInterval');
		const a = render(RelativeTime, { seconds: NOW - 50 });
		const b = render(RelativeTime, { seconds: NOW - 50 });
		const c = render(RelativeTime, { seconds: NOW - 50 });
		flushSync();
		expect(tickerSubscribers()).toBe(3);
		expect(spy).toHaveBeenCalledTimes(1);
		expect(a.container.textContent).toContain('50 seconds ago');
		vi.advanceTimersByTime(30_000);
		flushSync();
		expect(a.container.textContent).toContain('50 seconds ago');
		vi.advanceTimersByTime(30_000);
		flushSync();
		expect(a.container.textContent).toContain('1 minute ago');
		a.unmount();
		b.unmount();
		c.unmount();
		flushSync();
		expect(tickerSubscribers()).toBe(0);
		spy.mockRestore();
	});
});
