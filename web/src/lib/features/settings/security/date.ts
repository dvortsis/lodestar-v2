// A key's dates, as `formatDate` (`$lib/format`) shows them: medium date style, in the display
// time zone. Built here because importing `$lib/format` from Security's on-demand parts splits the
// format chunk other routes share and costs them size (decisions file); `date.test.ts` holds the
// two to the same output.
import { displayTimeZone } from '$lib/i18n/locale.svelte';

export function keyDate(seconds: number, locale: string): string {
	const at = new Date(seconds * 1000);
	try {
		return at.toLocaleDateString(locale, {
			dateStyle: 'medium',
			timeZone: displayTimeZone() || undefined
		});
	} catch {
		return at.toLocaleDateString(locale, { dateStyle: 'medium' });
	}
}
