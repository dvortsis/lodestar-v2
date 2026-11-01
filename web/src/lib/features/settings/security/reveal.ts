// Shows a new key's secret once (B12: Lodestar keeps only its hash). The secret goes straight from
// the create or regenerate answer into the dialog's props, and is gone when the dialog is: nothing
// keeps it in state, storage, a draft, a URL or the console.
import { mount, unmount } from 'svelte';

/** Long enough for the closing animation and focus restore before the dialog is removed. */
const UNMOUNT_MS = 400;

const open = new Set<() => void>();
/** Closed reveals still waiting out `UNMOUNT_MS`: each unmounts now, or when its timer fires. */
const closing = new Set<() => void>();

/** Closes every open reveal, its secret with it (the screen unmounted), and removes the ones
 * already closed but still waiting to unmount, so no timer outlives the screen. */
export function dismissReveals(): void {
	for (const close of [...open]) close();
	for (const flush of [...closing]) flush();
}

/** Loads the dialog. Call it before asking for the password: if it can't load, nothing is sent,
 * so no secret is made that can't be shown. */
export const loadReveal = () => import('./Reveal.svelte');

/** Resolves when the user closes the dialog. */
export async function revealSecret(name: string, secret: string): Promise<void> {
	const { default: Reveal } = await loadReveal();
	return new Promise<void>((resolve) => {
		const dismiss = () => {
			open.delete(dismiss);
			resolve();
			void unmount(app);
		};
		const app = mount(Reveal, {
			target: document.body,
			props: {
				name,
				secret,
				onclose: () => {
					if (!open.delete(dismiss)) return;
					resolve();
					const flush = () => {
						clearTimeout(timer);
						closing.delete(flush);
						void unmount(app);
					};
					const timer = setTimeout(flush, UNMOUNT_MS);
					closing.add(flush);
				}
			}
		});
		open.add(dismiss);
	});
}
