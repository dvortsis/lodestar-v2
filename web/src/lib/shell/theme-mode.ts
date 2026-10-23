/** The active theme's mode. The pre-paint script sets `color-scheme` on <html> before first paint. */
export function themeMode(): 'light' | 'dark' {
	if (typeof document === 'undefined') return 'light';
	return document.documentElement.style.colorScheme === 'dark' ? 'dark' : 'light';
}
