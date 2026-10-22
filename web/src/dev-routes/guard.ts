import { error } from '@sveltejs/kit';

// Test-only showcases (W7ab). They 404 unless the build sets VITE_DEV_ROUTES (Playwright's web
// server does; "0", "false", "off" and "no" count as off, as in scripts/dev-routes.mjs);
// `vite dev` always serves them.
const on = (v: unknown) => !!v && !['0', 'false', 'off', 'no'].includes(String(v).toLowerCase());

export function guardDevRoute() {
	if (!import.meta.env.DEV && !on(import.meta.env.VITE_DEV_ROUTES)) error(404, 'Not found');
}
