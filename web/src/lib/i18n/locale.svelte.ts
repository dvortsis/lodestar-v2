// The UI locale: settings first, then navigator.language, then en. The same value drives
// Paraglide messages, <html lang> and the number/date formatters (src/lib/format).
import { baseLocale, isLocale, locales, setLocale } from '$lib/paraglide/runtime.js';

export type AppLocale = (typeof locales)[number];

let current = $state<AppLocale>(baseLocale);

/** First supported locale in `preferences`: exact tag, then primary subtag ("zh-CN" → zh). */
export function matchLocale(preferences: readonly (string | null | undefined)[]): AppLocale | null {
	for (const tag of preferences) {
		if (!tag) continue;
		const lower = tag.toLowerCase();
		if (isLocale(lower)) return lower;
		const primary = lower.split('-')[0];
		if (primary && isLocale(primary)) return primary;
	}
	return null;
}

/** Settings value if it is a supported locale, else the browser's languages, else en. */
export function resolveLocale(
	setting: string | null | undefined,
	languages: readonly string[] = typeof navigator === 'undefined' ? [] : navigator.languages
): AppLocale {
	return matchLocale([setting]) ?? matchLocale(languages) ?? baseLocale;
}

/** Applies a locale to Paraglide, `<html lang>` and `currentLocale()`. */
export function applyLocale(locale: AppLocale): void {
	current = locale;
	setLocale(locale, { reload: false });
	if (typeof document !== 'undefined') document.documentElement.lang = locale;
}

/** Resolve and apply the locale; call at startup and whenever the language setting changes. */
export function initLocale(setting?: string | null): AppLocale {
	const locale = resolveLocale(setting);
	applyLocale(locale);
	return locale;
}

/** The active locale, for `Intl` formatters. Reactive in runes contexts. */
export function currentLocale(): AppLocale {
	return current;
}
