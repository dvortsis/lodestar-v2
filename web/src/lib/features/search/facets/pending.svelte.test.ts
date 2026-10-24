// The pending edit inside an effect root, as the host uses it (runes need a `.svelte` test file).
import { flushSync } from 'svelte';
import { expect, it } from 'vitest';
import type { Ast } from '$lib/state/search.svelte';
import { createPending } from './pending.svelte';

const word = (w: string): Ast => ({ op: 'word', field: 'any', word: w });

function setup(ast: Ast | null, text: string) {
	const w5 = $state({ ast, text });
	let p!: ReturnType<typeof createPending>;
	const stop = $effect.root(() => {
		p = createPending({ ast: () => w5.ast, text: () => w5.text });
	});
	flushSync();
	return { w5, p, stop };
}

it('keeps an edit until W5 adopts it', () => {
	const { w5, p, stop } = setup(word('lantern'), 'lantern');
	const edit = word('lantern res:1080p');
	p.set(edit);
	flushSync();
	expect(p.value).toBe(edit);
	w5.ast = edit;
	flushSync();
	expect(p.value).toBeNull();
	stop();
});

it('drops it when the text changes, even if the new text fails to parse too', () => {
	// A parse error (`ast` null): the click built on the rows' query. The person then types other
	// text that fails as well, so `ast` stays null: the edit must still go.
	const { w5, p, stop } = setup(null, 'lantern "');
	p.set(word('lantern res:1080p'));
	flushSync();
	w5.text = 'keeper "';
	flushSync();
	expect(w5.ast).toBeNull();
	expect(p.value).toBeNull();
	stop();
});

it('drops a refused edit, but not a newer one', () => {
	const { p, stop } = setup(word('a'), 'a');
	const first = word('a b');
	const second = word('a b c');
	p.set(first);
	p.set(second);
	p.drop(first);
	expect(p.value).toBe(second);
	p.drop(second);
	expect(p.value).toBeNull();
	stop();
});
