// The UI locale: settings first, then navigator.language, then en. The same value drives
// Paraglide messages, <html lang> and the number/date formatters (src/lib/format).
import { AUTO_LOCALE_COVERAGE, coverage } from './coverage.generated';
import { baseLocale, isLocale, locales, setLocale } from '$lib/paraglide/runtime.js';

export type AppLocale = (typeof locales)[number];

let current = $state<AppLocale>(baseLocale);

/** First supported locale in `preferences`: exact tag, then primary subtag ("zh-CN" → zh). */
export function matchLocale(
	preferences: readonly (string | null | undefined)[],
	covered: (locale: AppLocale) => boolean = () => true
): AppLocale | null {
	for (const tag of preferences) {
		if (!tag) continue;
		const lower = tag.toLowerCase();
		const primary = lower.split('-')[0];
		for (const candidate of [lower, primary]) {
			if (candidate && isLocale(candidate) && covered(candidate)) return candidate;
		}
	}
	return null;
}

/** A locale the browser may pick for the user: its catalogue must cover the UI (WCAG 3.1.1). */
const wellCovered = (locale: AppLocale) => (coverage[locale] ?? 0) >= AUTO_LOCALE_COVERAGE;

/** Settings value if supported, else the first well-covered browser language, else en. */
export function resolveLocale(
	setting: string | null | undefined,
	languages: readonly string[] = typeof navigator === 'undefined' ? [] : navigator.languages
): AppLocale {
	// An explicit setting always applies; the browser's language only when its catalogue is complete.
	return matchLocale([setting]) ?? matchLocale(languages, wellCovered) ?? baseLocale;
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
