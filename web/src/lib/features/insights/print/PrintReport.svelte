<script lang="ts">
	// The overview's one-page report (§10.9.6): print-only markup (a title and context line, and a
	// footer with the URL and generation time) plus print.css, which hides the app's chrome and the
	// levels that do not belong on the page. It loads after the overview mounts, so neither the markup
	// nor the CSS is in the route's first load. The theme is forced to Riverstone light while
	// printing (the compiler has no print tokens): dark themes would print as a black sheet.
	import { page } from '$app/state';
	import { formatDateFull } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import './print.css';

	interface Props {
		rangeText: string;
		compare: boolean;
		tz: string;
	}
	let { rangeText, compare, tz }: Props = $props();

	let now = $state(Math.floor(Date.now() / 1000));
	const generated = $derived(formatDateFull(now, currentLocale(), { timeZone: tz }));

	$effect(() => {
		const root = document.documentElement;
		let saved: { theme: string | undefined; style: string } | undefined;
		const before = () => {
			now = Math.floor(Date.now() / 1000);
			if (saved) return;
			saved = { theme: root.dataset.theme, style: root.getAttribute('style') ?? '' };
			root.dataset.theme = 'riverstone-light';
			root.removeAttribute('style');
			root.style.colorScheme = 'light';
		};
		const after = () => {
			if (!saved) return;
			root.dataset.theme = saved.theme;
			root.setAttribute('style', saved.style);
			saved = undefined;
		};
		const mq = matchMedia('print');
		const change = (e: MediaQueryListEvent) => (e.matches ? before() : after());
		window.addEventListener('beforeprint', before);
		window.addEventListener('afterprint', after);
		mq.addEventListener('change', change);
		if (mq.matches) before();
		return () => {
			window.removeEventListener('beforeprint', before);
			window.removeEventListener('afterprint', after);
			mq.removeEventListener('change', change);
			after();
		};
	});
</script>

<div class="ins-print-head" data-testid="insights-print-head">
	<p class="ins-print-title">{m.insights_print_title()}</p>
	<p>
		{m.insights_print_range({ range: rangeText })}{compare
			? ` · ${m.insights_print_compare()}`
			: ''}
		· {m.insights_overview_tz({ tz })}
	</p>
</div>
<p class="ins-print-foot" data-testid="insights-print-footer">
	{m.insights_print_footer({ url: page.url.href, time: generated })}
</p>
