// Svelte transition for the non-modal Drawer, which Bits UI does not render. Durations come from
// the motion tokens (design-tokens.md §9) so reduced motion (tokens at 0ms) needs no branch here
// beyond picking the crossfade.
import { cubicOut } from 'svelte/easing';

/** A px token such as `--drawer-min-width`, read once from the root; `fallback` where CSS isn't loaded (tests). */
export function tokenPx(name: string, fallback: number): number {
	if (typeof document === 'undefined') return fallback;
	const raw = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
	const n = raw.endsWith('px') ? parseFloat(raw) : NaN;
	return Number.isFinite(n) ? n : fallback;
}

export function isCoarsePointer(): boolean {
	return typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;
}

/** Fixed row height: `--row-height-compact`, raised to `--target-min` under a coarse pointer. */
export function defaultRowHeight(): number {
	const compact = tokenPx('--row-height-compact', 36);
	return isCoarsePointer() ? Math.max(compact, tokenPx('--target-min', 44)) : compact;
}

function tokenMs(name: string): number {
	if (typeof document === 'undefined') return 0;
	const raw = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
	return raw.endsWith('ms') ? parseFloat(raw) : raw.endsWith('s') ? parseFloat(raw) * 1000 : 0;
}

/** Slides in from the trailing edge (transform), or crossfades when the slide tokens are 0ms. */
export function drawerSlide(_node: Element, { out = false }: { out?: boolean } = {}) {
	const slide = tokenMs(out ? '--duration-drawer-out' : '--duration-drawer-in');
	if (slide > 0) {
		return {
			duration: slide,
			easing: cubicOut,
			css: (_t: number, u: number) => `transform: translateX(${u * 100}%)`
		};
	}
	return {
		duration: tokenMs('--duration-reduced-fade'),
		easing: cubicOut,
		css: (t: number) => `opacity: ${t}`
	};
}
