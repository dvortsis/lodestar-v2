// A reactive stand-in for `$app/state`'s `page`, so a test can move it from one error to the next.
import { SvelteURL } from 'svelte/reactivity';

export const page = $state<{ status: number; url: SvelteURL }>({
	status: 500,
	url: new SvelteURL('http://x/alerts')
});

export function setPage(status: number, path: string) {
	page.status = status;
	page.url = new SvelteURL(`http://x${path}`);
}
