// A reactive stand-in for `$app/state`'s `page`, for the unit tests.
/* eslint-disable svelte/prefer-svelte-reactivity -- a test stand-in, replaced as a whole */
export const page = $state({ url: new URL('http://localhost/compare') });
export const setUrl = (path: string) => (page.url = new URL(path, 'http://localhost'));
