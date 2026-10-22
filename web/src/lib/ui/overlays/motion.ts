// Svelte transition for the non-modal Drawer, which Bits UI does not render. Durations come from
// the motion tokens (design-tokens.md §9) so reduced motion (tokens at 0ms) needs no branch here
// beyond picking the crossfade.
import { cubicOut } from 'svelte/easing';

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
