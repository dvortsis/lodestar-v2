import { createRawSnippet } from 'svelte';

/** A children/slot snippet rendering plain text, for component tests. */
export const text = (value: string) =>
	createRawSnippet(() => ({ render: () => `<span>${value}</span>` }));

/** A snippet rendering an arbitrary HTML string. */
export const html = (value: string) => createRawSnippet(() => ({ render: () => value }));
