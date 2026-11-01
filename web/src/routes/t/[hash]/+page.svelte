<script lang="ts">
	// /t/{hash} as a full page (a direct link, or "Open full page"). From Search it is a drawer
	// instead (features/detail/DetailHost). The column is capped at --manuscript-width.
	import { onMount, untrack } from 'svelte';
	import { showBidi } from '$lib/util/bidi';
	import { m } from '$lib/paraglide/messages.js';
	import DetailActions from '$lib/features/detail/DetailActions.svelte';
	import DetailView from '$lib/features/detail/DetailView.svelte';
	import { setDrawerTarget } from '$lib/features/detail/target';
	import { DetailModel } from '$lib/features/detail/model.svelte';
	import '$lib/features/detail/detail.css';

	let { data } = $props();
	const model = new DetailModel();

	$effect(() => {
		untrack(() => model.load(data.hash));
		setDrawerTarget(data.hash);
		return () => {
			model.destroy();
			setDrawerTarget(null);
		};
	});
	onMount(
		() =>
			void import('$lib/features/detail/commands').then(
				(mod) => mod.registerDetailCommands(),
				() => {}
			)
	);
</script>

<svelte:head><title>{showBidi(model.title) || m.torrent()} · Lodestar</title></svelte:head>

<div class="ls-detail-page">
	<header>
		<h1>{showBidi(model.title) || m.torrent()}</h1>
		<div class="ls-detail-row"><DetailActions detail={model.detail} /></div>
	</header>
	<DetailView {model} mode="page" />
</div>
