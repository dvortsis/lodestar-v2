import { initLocale } from '$lib/i18n/locale.svelte';

export const ssr = false;
export const prerender = false;

// Settings aren't wired yet; null falls back to navigator.language, then en.
initLocale(null);
