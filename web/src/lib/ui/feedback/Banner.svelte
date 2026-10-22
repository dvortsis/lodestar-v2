<script lang="ts">
	import type { Snippet } from 'svelte';
	import IconInfo from '~icons/ph/info';
	import IconSuccess from '~icons/ph/check-circle';
	import IconWarning from '~icons/ph/warning';
	import IconError from '~icons/ph/x-circle';
	import { m } from '$lib/paraglide/messages.js';

	type Kind = 'info' | 'success' | 'warning' | 'error';

	interface Props {
		kind?: Kind;
		/** Optional bold lead-in. */
		title?: string;
		children: Snippet;
		/** One optional action (a Button). */
		action?: Snippet;
		class?: string;
	}

	let { kind = 'info', title, children, action, class: className = '' }: Props = $props();

	const icons = { info: IconInfo, success: IconSuccess, warning: IconWarning, error: IconError };
	const ink = {
		info: 'text-info',
		success: 'text-success',
		warning: 'text-warning',
		error: 'text-error'
	};
	const border = {
		info: 'border-info',
		success: 'border-success',
		warning: 'border-warning',
		error: 'border-error'
	};
	const kindLabel = $derived(
		{
			info: m.common_info,
			success: m.common_success,
			warning: m.common_warning,
			error: m.common_error
		}[kind]()
	);
	const Icon = $derived(icons[kind]);
</script>

<div
	role={kind === 'error' ? 'alert' : 'status'}
	class="flex items-start gap-3 rounded-box border bg-base-100 p-3 text-body text-base-content {border[
		kind
	]} {className}"
>
	<Icon class="mt-0.5 size-5 shrink-0 {ink[kind]}" aria-hidden="true" />
	<div class="min-w-0 flex-1">
		<span class="sr-only">{kindLabel}:&nbsp;</span>
		{#if title}<p class="font-medium">{title}</p>{/if}
		<div>{@render children()}</div>
	</div>
	{#if action}<div class="shrink-0">{@render action()}</div>{/if}
</div>
