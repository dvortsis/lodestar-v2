<script lang="ts">
	// Where the Related section mounts (one line in DetailView). Its chunk loads when the section
	// scrolls near in the drawer, at once on the full page, so the drawer's first load carries only
	// this placeholder (§10.1).
	import { m } from '$lib/paraglide/messages.js';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import type { SearchHit, TorrentDetail } from '../cache';
	import { whenVisible } from '../visible';

	interface Props {
		detail: TorrentDetail;
		hit: SearchHit | null;
		level?: number;
		mode: 'drawer' | 'page';
	}
	let { detail, hit, level = 3, mode }: Props = $props();

	let Related = $state.raw<typeof import('./Related.svelte').default>();
	let asked = false;
	function load() {
		if (asked) return;
		asked = true;
		void import('./Related.svelte').then((mod) => (Related = mod.default));
	}
	$effect(() => {
		if (mode === 'page') load();
	});
</script>

<div use:whenVisible={load} data-section="related" data-testid="detail-related-mount">
	{#if Related}
		<Related {detail} {hit} {level} {mode} />
	{:else}
		<div class="ls-detail-section" aria-busy="true">
			<div role="heading" aria-level={level}>{m.detail_related_title()}</div>
			<Skeleton lines={3} />
		</div>
	{/if}
</div>
