<script lang="ts">
	import { formatDateTime, formatRelative } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { subscribeTicker, ticker } from './ticker.svelte';

	interface Props {
		/** Unix seconds; null renders "—". */
		seconds: number | null | undefined;
		/** IANA time zone setting for the absolute time. */
		timeZone?: string;
		class?: string;
	}

	let { seconds, timeZone, class: className = '' }: Props = $props();

	$effect(() => subscribeTicker());

	const locale = $derived(currentLocale());
	const relative = $derived(formatRelative(seconds, locale, { now: ticker.now }));
	const absolute = $derived(formatDateTime(seconds, locale, { timeZone }));
	const iso = $derived(
		typeof seconds === 'number' && Number.isFinite(seconds)
			? new Date(seconds * 1000).toISOString()
			: undefined
	);
</script>

<!-- title is the sighted tooltip; the sr-only absolute time is the accessible text (not hover-only). -->
<time datetime={iso} title={iso ? absolute : undefined} class="tabular-nums {className}">
	{relative}
	{#if iso}<span class="sr-only">({absolute})</span>{/if}
</time>
