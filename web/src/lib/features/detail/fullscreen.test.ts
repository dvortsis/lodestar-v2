import { afterEach, expect, it } from 'vitest';
import { coverBackground } from './fullscreen';

afterEach(() => {
	document.body.replaceChildren();
});

it('covers ancestor siblings and restores only inert states it owns', () => {
	document.body.innerHTML =
		'<header></header><main><section id="results"></section><div id="host"><button>Close</button></div></main><footer></footer>';
	const header = document.querySelector('header')!;
	header.inert = true;
	const host = document.querySelector<HTMLElement>('#host')!;
	const results = document.querySelector<HTMLElement>('#results')!;
	const restore = coverBackground(host);
	expect(results.inert).toBe(true);
	expect(document.querySelector('footer')!.inert).toBe(true);
	expect(host.inert).not.toBe(true);
	expect(host.parentElement!.inert).not.toBe(true);
	restore();
	expect(results.inert).toBe(false);
	expect(header.inert).toBe(true);
	expect(document.querySelector('footer')!.inert).toBe(false);
});
