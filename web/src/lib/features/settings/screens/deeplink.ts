// Deep links (`#crawler.intensity`, §10.10.2): open the field's Advanced, scroll it into view,
// focus its control and highlight it briefly. Ids contain a dot, so they are looked up with
// `getElementById`, never as a selector.
import type { FieldKey } from '../form/types';

/** How long the highlight stays; the stylesheet fades it, or holds a static ring under reduced motion. */
export const HIGHLIGHT_MS = 2000;

/** The field a URL hash names, when this form has it. */
export function fieldFromHash(hash: string, keys: readonly FieldKey[]): FieldKey | undefined {
	let name: string;
	try {
		name = decodeURIComponent(hash.replace(/^#/, ''));
	} catch {
		return undefined;
	}
	return keys.find((k) => k === name);
}

const CONTROL =
	'input:checked, input:not([type="radio"]):not([type="hidden"]), select, textarea, button, [tabindex="0"]';

/** The control a field's label points at, else the first usable one in its control area. */
export function controlOf(wrapper: HTMLElement): HTMLElement | null {
	const labelled = wrapper.querySelector('label[for]')?.getAttribute('for');
	const target = labelled ? document.getElementById(labelled) : null;
	if (target instanceof HTMLElement) return target;
	return wrapper.querySelector<HTMLElement>(`[data-control] :is(${CONTROL})`);
}

/** How long to wait for a control that loads on demand (by type) before giving up on focusing it. */
export const CONTROL_WAIT_MS = 3000;

/** Resolves with the field's control once it is in the DOM (some load on demand), or null. */
function controlWhenReady(wrapper: HTMLElement): Promise<HTMLElement | null> {
	return new Promise((resolve) => {
		const found = controlOf(wrapper);
		if (found) return resolve(found);
		const timer = setTimeout(done, CONTROL_WAIT_MS);
		const observer = new MutationObserver(() => {
			const c = controlOf(wrapper);
			if (c) done(c);
		});
		function done(c?: HTMLElement | null) {
			clearTimeout(timer);
			observer.disconnect();
			resolve(c ?? controlOf(wrapper));
		}
		observer.observe(wrapper, { childList: true, subtree: true });
	});
}

/** Scrolls to a field, focuses its control and marks it for a moment. False when it isn't there. */
export function showField(key: FieldKey): boolean {
	const wrapper = document.getElementById(key);
	if (!wrapper) return false;
	wrapper.scrollIntoView?.({ block: 'center' });
	void controlWhenReady(wrapper).then((c) => c?.focus({ preventScroll: true }));
	wrapper.setAttribute('data-highlight', '');
	setTimeout(() => wrapper.removeAttribute('data-highlight'), HIGHLIGHT_MS);
	return true;
}
