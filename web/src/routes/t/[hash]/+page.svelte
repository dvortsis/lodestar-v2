<script lang="ts">
	import { PAGE_BODY_WIDE, PAGE_FRAME, PAGE_H1 } from '$lib/ui/layout/classes';
	// /t/{hash} as a full page (a direct link, or "Open full page"). From Search it is a drawer
	// instead (features/detail/DetailHost). The page is the shared frame (ui/layout), up to 80 rem; its header and summary keep --manuscript-width.
	import { onMount, untrack } from 'svelte';
	import { resolve } from '$app/paths';
	import { showBidi } from '$lib/util/bidi';
	import { m } from '$lib/paraglide/messages.js';
	import DetailActions from '$lib/features/detail/DetailActions.svelte';
	import DetailView from '$lib/features/detail/DetailView.svelte';
	import { setDrawerTarget } from '$lib/features/detail/target';
	import { DetailModel } from '$lib/features/detail/model.svelte';
	import '$lib/features/detail/detail.css';
	import '$lib/features/detail/page.css';

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
	// Back goes where the visitor came from; a direct link (no earlier page of ours) goes to Search.
	function back(event: MouseEvent) {
		if (event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey)
			return;
		let ours = false;
		try {
			ours =
				history.length > 1 &&
				!!document.referrer &&
				new URL(document.referrer).origin === location.origin;
		} catch {
			// an unreadable referrer: the link's own href (Search) is used
		}
		if (!ours) return;
		event.preventDefault();
		history.back();
	}
	onMount(
		() =>
			void import('$lib/features/detail/commands').then(
				(mod) => mod.registerDetailCommands(),
				() => {}
			)
	);
</script>

<svelte:head><title>{showBidi(model.title) || m.torrent()} · Lodestar</title></svelte:head>

<div class={PAGE_FRAME} data-page-frame="">
	<div class={PAGE_BODY_WIDE}>
		<div class="contents ls-detail-page">
			<a
				class="ls-focus link inline-flex items-center gap-1 text-body"
				href={resolve('/search')}
				onclick={back}
				data-testid="detail-back-search">{m.detail_back_search()}</a
			>
			<header
				class="flex max-w-(--manuscript-width) flex-wrap items-start justify-between gap-x-4 gap-y-2"
			>
				<h1 class={PAGE_H1} tabindex="-1">{showBidi(model.title) || m.torrent()}</h1>
				<div class="ls-detail-row basis-full medium:basis-auto">
					<DetailActions detail={model.detail} mode="page" />
				</div>
			</header>
			<DetailView {model} mode="page" />
		</div>
	</div>
</div>
