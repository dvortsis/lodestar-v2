// See https://svelte.dev/docs/kit/types#app.d.ts
declare global {
	namespace App {
		interface PageState {
			detail?: { hash: string };
		}
	}
}

declare global {
	// Set by lib/i18n/locale.svelte.ts: the display time zone, for lib/format (which can't import it).
	// eslint-disable-next-line no-var
	var lodestarZone: (() => string) | undefined;
}

export {};
