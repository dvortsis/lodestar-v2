<script lang="ts">
	// /t/{hash} as a full page (a direct link, or "Open full page"). From Search it is a drawer
	// instead (features/detail/DetailHost). The column is capped at --manuscript-width.
	import { onMount, untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import DetailActions from '$lib/features/detail/DetailActions.svelte';
	import DetailView from '$lib/features/detail/DetailView.svelte';
	import ManualMagnet from '$lib/features/detail/ManualMagnet.svelte';
	import { registerDetailCommands, setDrawerTarget } from '$lib/features/detail/commands';
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
	onMount(() => void registerDetailCommands());
</script>

<svelte:head><title>{model.title || m.torrent()} · Lodestar</title></svelte:head>

<div class="ls-detail-page">
	<header>
		<h1>{model.title || m.torrent()}</h1>
		<div class="ls-detail-row"><DetailActions detail={model.detail} /></div>
	</header>
	<DetailView {model} mode="page" />
</div>
<ManualMagnet />
