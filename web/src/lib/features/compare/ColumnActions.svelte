<script lang="ts">
	import IconOpen from '~icons/ph/arrow-square-out';
	import IconCopy from '~icons/ph/copy';
	import IconRemove from '~icons/ph/x';
	import Button from '$lib/ui/controls/Button.svelte';
	import { FOCUS_RING, TARGET_MIN } from '$lib/ui/classes';
	import { m } from '$lib/paraglide/messages.js';
	import { torrentHref } from './href';
	import { headingOf, type CompareTorrent } from './model';

	interface Props {
		torrent: CompareTorrent;
		onCopy: (hash: string) => void;
		onPrefetch: (hash: string) => void;
		onRemove: (hash: string) => void;
	}
	let { torrent, onCopy, onPrefetch, onRemove }: Props = $props();

	const href = $derived(torrentHref(torrent.hash));
	const name = $derived(headingOf(torrent));
	const prefetch = () => onPrefetch(torrent.hash);
</script>

<!-- The magnet is fetched on hover or focus, so Copy is usually instant (§10.1's prefetch idea). -->
<div
	class="cmp-actions"
	role="group"
	aria-label={name}
	data-hash={torrent.hash}
	onpointerenter={prefetch}
	onfocusin={prefetch}
>
	{#if href}
		<!-- eslint-disable svelte/no-navigation-without-resolve -->
		<a
			{href}
			class="btn btn-ghost btn-sm {FOCUS_RING} {TARGET_MIN}"
			aria-label={m.compare_open_for({ name })}
			data-first-action
		>
			<IconOpen aria-hidden="true" class="size-4" />{m.compare_open()}
		</a>
		<!-- eslint-enable svelte/no-navigation-without-resolve -->
	{/if}
	<Button
		variant="quiet"
		dense
		aria-label={m.compare_copy_for({ name })}
		onclick={() => onCopy(torrent.hash)}
		data-first-action={href ? undefined : ''}
	>
		<IconCopy aria-hidden="true" class="size-4" />{m.compare_copy_magnet()}
	</Button>
	<Button
		variant="quiet"
		dense
		aria-label={m.compare_remove_for({ name })}
		onclick={() => onRemove(torrent.hash)}
	>
		<IconRemove aria-hidden="true" class="size-4" />{m.compare_remove()}
	</Button>
</div>

<style>
	.cmp-actions {
		display: flex;
		flex-wrap: wrap;
		gap: calc(var(--spacing) * 1);
	}
</style>
