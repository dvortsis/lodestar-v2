// The module scripts/licenses.ts provides (referenced from there, so every program that loads
// vite.config.ts sees it). S-about fetches the URL when Licences and credits opens.
declare module 'virtual:licenses' {
	/** The content-hashed `licenses-<hash>.json` (the dev server's copy under `vite dev`). */
	export const licensesUrl: string;
}
