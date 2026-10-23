// Opening focus for the two dialogs. Bits UI puts focus on the first control (the close button)
// when the panel mounts, and the invoker may still hold it a moment before: a single timeout can
// land before or after that. So the target is tried a few times, but only while focus is still
// somewhere nobody chose (the page body, the panel itself or its close button), never over a
// control the person has moved to.
const TRIES_MS = [0, 50, 150, 400];

export function focusAfterOpen(target: () => HTMLElement | null | undefined): () => void {
	const timers = TRIES_MS.map((ms) =>
		setTimeout(() => {
			const el = target();
			if (!el || document.activeElement === el) return;
			const active = document.activeElement;
			const unchosen =
				!active ||
				active === document.body ||
				active.getAttribute('role') === 'dialog' ||
				active.hasAttribute('data-dialog-close') ||
				// Still on the invoker, outside the panel.
				!el.closest('[role="dialog"]')?.contains(active);
			if (unchosen) el.focus();
		}, ms)
	);
	return () => timers.forEach(clearTimeout);
}
