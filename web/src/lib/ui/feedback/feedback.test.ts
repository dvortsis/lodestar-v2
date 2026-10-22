import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { flushSync } from 'svelte';
import { html, text } from '../testing';
import Banner from './Banner.svelte';
import Skeleton from './Skeleton.svelte';
import EmptyState from './EmptyState.svelte';
import Toaster from './Toaster.svelte';
import { toast, toasts } from './toasts.svelte';

describe('Banner', () => {
	it('each kind has an icon and text; error is an alert, others are status', () => {
		for (const kind of ['info', 'success', 'warning', 'error'] as const) {
			const { container, unmount } = render(Banner, { kind, children: text('Body') });
			const root = screen.getByRole(kind === 'error' ? 'alert' : 'status');
			expect(root.querySelector('svg[aria-hidden="true"]')).not.toBeNull();
			expect(container.querySelector('.sr-only')!.textContent).toMatch(/^\w+:\u00a0$/);
			unmount();
		}
	});

	it('renders the optional title and action', () => {
		render(Banner, {
			kind: 'warning',
			title: 'Stale',
			children: text('Rollups are late'),
			action: html('<button>Retry</button>')
		});
		expect(screen.getByText('Stale')).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy();
	});
});

describe('Skeleton', () => {
	it('is hidden from assistive tech and animates only without reduced motion', () => {
		const { getByTestId } = render(Skeleton, { lines: 3 });
		const root = getByTestId('skeleton');
		expect(root.getAttribute('aria-hidden')).toBe('true');
		expect(root.children).toHaveLength(3);
		expect(root.innerHTML).toContain('motion-safe:animate-pulse');
		expect(root.innerHTML).not.toMatch(/(^|\s)animate-pulse/);
	});

	it('renders a single block', () => {
		const { getByTestId } = render(Skeleton, { class: 'h-8' });
		expect(getByTestId('skeleton').className).toContain('motion-safe:animate-pulse');
	});
});

describe('EmptyState', () => {
	it('has a title, text, one action and an optional illustration', () => {
		render(EmptyState, {
			title: 'No saved searches',
			text: 'Save a search to get alerts.',
			action: html('<button>Save search</button>'),
			illustration: html('<svg data-testid="art"></svg>')
		});
		expect(screen.getByRole('heading', { level: 2, name: 'No saved searches' })).toBeTruthy();
		expect(screen.getByText('Save a search to get alerts.')).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Save search' })).toBeTruthy();
		expect(screen.getByTestId('art').parentElement!.getAttribute('aria-hidden')).toBe('true');
	});

	it('heading level is configurable; illustration and action are optional', () => {
		render(EmptyState, { title: 'Nothing', headingLevel: 3 });
		expect(screen.getByRole('heading', { level: 3 })).toBeTruthy();
		expect(screen.queryByRole('button')).toBeNull();
	});
});

describe('Toaster', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		toast.clear();
	});
	afterEach(() => {
		toast.clear();
		vi.useRealTimers();
	});

	it('is a polite live region present before any toast', () => {
		render(Toaster);
		const region = screen.getByRole('region', { name: 'Notifications' });
		expect(region.getAttribute('aria-live')).toBe('polite');
	});

	it('queues toasts, shows text with kind, and never moves focus', () => {
		render(Toaster);
		const before = document.activeElement;
		toast.push({ text: 'Saved', kind: 'success' });
		toast.push({ text: 'Copied' });
		flushSync();
		expect(screen.getAllByTestId('toast')).toHaveLength(2);
		expect(screen.getByText('Saved').parentElement!.textContent).toContain('Success:\u00a0Saved');
		expect(document.activeElement).toBe(before);
	});

	it('dismiss button has a name and removes the toast', async () => {
		render(Toaster, { dismissLabel: 'Close notification' });
		toast.push({ text: 'Saved' });
		flushSync();
		await fireEvent.click(screen.getByRole('button', { name: 'Close notification' }));
		expect(screen.queryAllByTestId('toast')).toHaveLength(0);
	});

	it('auto-dismisses, pauses while hovered, and keeps errors until dismissed', async () => {
		render(Toaster);
		toast.push({ text: 'Soon', timeout: 1000 });
		toast.push({ text: 'Broken', kind: 'error' });
		flushSync();
		const soon = screen.getByText('Soon').closest('[data-testid="toast"]')!;
		await fireEvent.pointerEnter(soon);
		vi.advanceTimersByTime(5000);
		flushSync();
		expect(screen.queryByText('Soon')).not.toBeNull();
		await fireEvent.pointerLeave(soon);
		vi.advanceTimersByTime(1000);
		flushSync();
		expect(screen.queryByText('Soon')).toBeNull();
		vi.advanceTimersByTime(60_000);
		flushSync();
		expect(screen.queryByText('Broken')).not.toBeNull();
	});

	it('stays while focus is inside, even after hover enters and leaves', async () => {
		render(Toaster, { dismissLabel: 'Close' });
		toast.push({ text: 'Held', timeout: 1000 });
		flushSync();
		const el = screen.getByText('Held').closest('[data-testid="toast"]')!;
		const close = screen.getByRole('button', { name: 'Close' });
		close.focus();
		await fireEvent.focusIn(close);
		await fireEvent.pointerEnter(el);
		await fireEvent.pointerLeave(el);
		vi.advanceTimersByTime(10_000);
		flushSync();
		expect(screen.queryByText('Held')).not.toBeNull();
		await fireEvent.focusOut(close);
		vi.advanceTimersByTime(1000);
		flushSync();
		expect(screen.queryByText('Held')).toBeNull();
		expect(vi.getTimerCount()).toBe(0);
	});

	it('dismissing the toast that holds focus moves focus on, never to <body>', async () => {
		const outside = document.createElement('button');
		document.body.append(outside);
		render(Toaster, { dismissLabel: 'Close' });
		toast.push({ text: 'One', timeout: 0 });
		toast.push({ text: 'Two', timeout: 0 });
		flushSync();
		const [first, second] = screen.getAllByRole('button', { name: 'Close' });
		// Focus enters the region from `outside`, then the first toast is dismissed.
		outside.focus();
		await fireEvent.focusIn(first!, { relatedTarget: outside });
		first!.focus();
		await fireEvent.click(first!);
		await Promise.resolve();
		await Promise.resolve();
		expect(document.activeElement).toBe(second);
		// Dismissing the last one returns focus to where it came from.
		await fireEvent.click(second!);
		await Promise.resolve();
		await Promise.resolve();
		expect(document.activeElement).toBe(outside);
		outside.remove();
	});

	it('caps the queue, dropping the oldest non-error first', () => {
		toast.push({ text: 'err', kind: 'error' });
		for (let i = 0; i < 5; i++) toast.push({ text: `t${i}` });
		expect(toasts.items).toHaveLength(4);
		expect(toasts.items[0]!.text).toBe('err');
		expect(toasts.items.map((t) => t.text)).toEqual(['err', 't2', 't3', 't4']);
	});
});
