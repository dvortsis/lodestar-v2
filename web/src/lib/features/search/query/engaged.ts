// Work the bare page doesn't need on first paint waits for the first interaction (§10.1; the same
// signals the shell's chrome waits for, W9 decisions), so Lighthouse's load never fetches it.
const ENGAGED = ['pointerdown', 'keydown', 'touchstart', 'focusin'] as const;

/**
 * Runs `cb` once, on the first interaction anywhere on the page. Returns the cancel. Focus on
 * <body> is not one: kit puts it there itself when the app starts.
 */
export function onFirstInteraction(cb: () => void): () => void {
	const stop = () => {
		for (const e of ENGAGED) removeEventListener(e, go, true);
	};
	const go = (e: Event) => {
		if (e.type === 'focusin' && e.target === document.body) return;
		stop();
		cb();
	};
	for (const e of ENGAGED) addEventListener(e, go, { capture: true, passive: true });
	return stop;
}
