import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { html, text } from '../testing';
import PageFrame from './PageFrame.svelte';
import PageHeader from './PageHeader.svelte';

describe('PageFrame', () => {
	it('is a left-aligned container with the shared gutters and renders its children', () => {
		const { container } = render(PageFrame, { children: text('Body') });
		const frame = container.querySelector<HTMLElement>('[data-page-frame]')!;
		expect(frame.className).toContain('px-4');
		expect(frame.className).toContain('large:px-6');
		expect(frame.className).not.toMatch(/\bmx-auto\b/);
		expect(frame.textContent).toBe('Body');
	});

	it('caps the content by size and never the gutters', () => {
		const inner = (size?: 'full' | 'wide' | 'manuscript') => {
			const { container, unmount } = render(PageFrame, { size, children: text('x') });
			const cls = container.querySelector<HTMLElement>('[data-page-frame] > div')!.className;
			unmount();
			return cls;
		};
		expect(inner('full')).not.toContain('max-w');
		expect(inner('wide')).toContain('max-w-7xl');
		expect(inner()).toContain('max-w-7xl');
		expect(inner('manuscript')).toContain('max-w-(--manuscript-width)');
	});
});

describe('PageHeader', () => {
	it('renders one h1, with whatever the page puts after it', () => {
		render(PageHeader, { title: 'Alerts', children: html('<button>New</button>') });
		expect(screen.getAllByRole('heading', { level: 1 })).toHaveLength(1);
		expect(screen.getByRole('heading', { name: 'Alerts' })).toBeTruthy();
		expect(screen.getByRole('button', { name: 'New' })).toBeTruthy();
	});

	it('keeps its id and is a script focus target, not a tab stop', () => {
		render(PageHeader, { title: 'A', id: 'a-h' });
		const h1 = screen.getByRole('heading', { level: 1 });
		expect(h1.id).toBe('a-h');
		expect(h1.getAttribute('tabindex')).toBe('-1');
		h1.focus();
		expect(document.activeElement).toBe(h1);
	});
});
