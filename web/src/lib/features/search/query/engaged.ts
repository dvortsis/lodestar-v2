// Work the bare page doesn't need on first paint waits for the first interaction (§10.1; the same
// signals the shell's chrome waits for, W9 decisions), so Lighthouse's load never fetches it.
const ENGAGED = ['pointerdown', 'keydown', 'touchstart', 'focusin'] as const;

/**
 * Runs `cb` once, on the first interaction anywhere on the page, or after `afterMs` if given (the
 * shell's 30 s fallback). Returns the cancel. Focus on <body> is not an interaction: kit puts it
 * there itself when the app starts.
 */
export function onFirstInteraction(cb: () => void, afterMs?: number): () => void {
	const timer = afterMs === undefined ? undefined : setTimeout(() => go(), afterMs);
	const stop = () => {
		clearTimeout(timer);
		for (const e of ENGAGED) removeEventListener(e, go, true);
	};
	const go = (e?: Event) => {
		if (e?.type === 'focusin' && e.target === document.body) return;
		stop();
		cb();
	};
	for (const e of ENGAGED) addEventListener(e, go, { capture: true, passive: true });
	return stop;
}
