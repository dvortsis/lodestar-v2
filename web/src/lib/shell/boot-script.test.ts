// The inline script in app.html decides whether the static English shell stays. It must agree with
// resolveLocale (lib/i18n/locale.svelte.ts): drop the shell only when the app will show another
// language. The covered-locale table in it is baked in by scripts/messages.ts.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { AUTO_LOCALE_COVERAGE, coverage } from '$lib/i18n/coverage.generated';

const html = readFileSync(resolve(process.cwd(), 'src/app.html'), 'utf8');
const script = html
	.split('<script')
	.map((part) => part.slice(part.indexOf('>') + 1, part.indexOf('</script>')))
	.find((body) => body.includes('boot-locales'))!;

function run(languages: string[], path = '/search', table?: string): boolean {
	document.body.innerHTML =
		'<div id="boot"><a data-boot-brand href="/search"></a><a data-boot-nav href="/search"></a><h1 id="boot-h1" hidden></h1></div>';
	Object.defineProperty(navigator, 'languages', { value: languages, configurable: true });
	history.replaceState(null, '', path);
	const code = table
		? script.replace(/\/\*boot-locales\*\/[^]*?\/\*end\*\//, `/*boot-locales*/${table}/*end*/`)
		: script;
	new Function(code)();
	return document.getElementById('boot') !== null;
}

afterEach(() => history.replaceState(null, '', '/'));

describe('app.html boot script', () => {
	it('bakes exactly the locales the app auto-selects', () => {
		const baked = JSON.parse(/\/\*boot-locales\*\/([^]*?)\/\*end\*\//.exec(script)![1]!);
		const expected = Object.entries(coverage)
			.filter(([locale, share]) => locale === 'en' || share >= AUTO_LOCALE_COVERAGE)
			.map(([locale]) => locale)
			.sort();
		expect(Object.keys(baked).sort()).toEqual(expected);
	});

	it('keeps the shell for English and for a language the app will not auto-select', () => {
		expect(run(['en-US'])).toBe(true);
		expect(run([])).toBe(true);
		expect(run(['zh-CN'], '/search', '{"en":1}')).toBe(true);
		expect(run(['ja', 'zh-CN'], '/search', '{"en":1}')).toBe(true);
	});

	it('drops the shell when the first covered language is not English', () => {
		const both = '{"en":1,"zh":1}';
		expect(run(['zh-CN'], '/search', both)).toBe(false);
		expect(run(['ja', 'zh-CN'], '/search', both)).toBe(false);
		expect(run(['en-US', 'zh-CN'], '/search', both)).toBe(true);
		expect(run(['zh-Hans-CN'], '/search', both)).toBe(false);
	});

	it('drops the shell on the sign-in screens', () => {
		for (const path of ['/login', '/setup', '/welcome']) expect(run(['en-US'], path)).toBe(false);
	});

	it('keeps ?density on the links and marks the current destination', () => {
		history.replaceState(null, '', '/search?density=compact');
		run(['en'], '/search?density=compact');
		expect(document.querySelector('[data-boot-brand]')!.getAttribute('href')).toBe(
			'/search?density=compact'
		);
		expect(document.querySelector('[data-boot-nav]')!.getAttribute('aria-current')).toBe('page');
		expect(document.getElementById('boot-h1')!.hidden).toBe(false);
	});
});
