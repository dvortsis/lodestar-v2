// Every navigation the auth flows make. Static pages go through resolve(); `next` and the login
// URL carry the user's own path (safeNext's output), which is a URL, not a route id.
import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { loginUrl } from './next';

const replace = { replaceState: true };

export const toSetup = () => goto(resolve('/setup'), replace);
export const toLogin = () => goto(resolve('/login'), replace);
export const toWelcome = () => goto(resolve('/welcome'), replace);

/** `next` must come from safeNext: a same-origin path that isn't an auth page. */
// eslint-disable-next-line svelte/no-navigation-without-resolve
export const toNext = (next: string) => goto(next, replace);

/** After a 401: the sign-in page, returning to `next` (pushes, so Back leaves the login). */
// eslint-disable-next-line svelte/no-navigation-without-resolve
export const toLoginFrom = (next: string) => goto(loginUrl(next));
