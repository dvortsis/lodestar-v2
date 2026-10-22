import { error } from '@sveltejs/kit';

// Test-only route (W7cd): the primitive showcase for e2e and axe. It 404s unless the build sets
// VITE_DEV_ROUTES=1 (the Playwright web server does); `vite dev` always serves it.
export const load = () => {
	if (!import.meta.env.DEV && !import.meta.env.VITE_DEV_ROUTES) error(404, 'Not found');
};
