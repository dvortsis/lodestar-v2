import { afterEach, describe, expect, it, vi } from 'vitest';
import { revealSection } from './section.svelte';

const WAIT = { timeout: 10_000 };

function drawer() {
	const root = document.createElement('div');
	root.innerHTML = '<div data-section="related"><div role="heading">Related</div></div>';
	document.body.append(root);
	return root;
}
const mounted = (root: HTMLElement) => {
	const h = document.createElement('div');
	h.setAttribute('role', 'heading');
	h.tabIndex = -1;
	h.textContent = 'Related';
	root.querySelector('[data-section="related"]')!.replaceChildren(h);
	return h;
};

afterEach(() => (document.body.innerHTML = ''));

describe('revealSection', () => {
	it('scrolls to the section and focuses its heading once it has mounted', async () => {
		const root = drawer();
		const scroll = vi.fn();
		root.querySelector('[data-section="related"]')!.scrollIntoView = scroll;
		revealSection(root, 'related');
		expect(scroll).toHaveBeenCalledWith({ block: 'start' });
		expect(document.activeElement).toBe(document.body); // the placeholder takes no focus
		const heading = mounted(root);
		await vi.waitFor(() => expect(document.activeElement).toBe(heading), WAIT);
	});

	it('focuses at once when the heading is already there', () => {
		const root = drawer();
		const heading = mounted(root);
		revealSection(root, 'related');
		expect(document.activeElement).toBe(heading);
	});

	it('stops waiting when cancelled', async () => {
		const root = drawer();
		const stop = revealSection(root, 'related');
		stop();
		mounted(root);
		await new Promise((r) => setTimeout(r, 0));
		expect(document.activeElement).toBe(document.body);
	});

	it('gives up after 5 s, and when focus has moved elsewhere', () => {
		vi.useFakeTimers();
		try {
			const root = drawer();
			revealSection(root, 'related');
			vi.advanceTimersByTime(5001);
			const late = mounted(root);
			return vi
				.waitFor(() => Promise.resolve())
				.then(() => {
					expect(document.activeElement).not.toBe(late);
				});
		} finally {
			vi.useRealTimers();
		}
	});

	it('does not take focus from where the user moved to', async () => {
		const root = drawer();
		const other = document.createElement('button');
		document.body.append(other);
		revealSection(root, 'related');
		other.focus();
		mounted(root);
		await Promise.resolve();
		await new Promise((r) => queueMicrotask(() => r(null)));
		expect(document.activeElement).toBe(other);
	});

	it('does nothing when the drawer has no such section', () => {
		const root = document.createElement('div');
		expect(() => revealSection(root, 'related')()).not.toThrow();
	});
});
