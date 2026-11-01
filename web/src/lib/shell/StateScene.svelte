<script lang="ts">
	// A state illustration (§10.19): decorative (`alt=""`), the variant follows the active theme's
	// mode, a 12 px frame, no animation, lazy. The parent puts the copy and the next step first in
	// the DOM. The slot in StateScreen hides it under 600 px of viewport height.
	import { scenes, type SceneId } from '$lib/brand/scenes.generated';
	import { themeMode } from './theme-mode';

	let { id }: { id: SceneId } = $props();

	// Off when the account turned them off or the browser's data saver is on. Not rendered, so no
	// image is fetched. Read from the per-browser cache (written by `applyLook` on every change)
	// when a scene is built. With no cache (a first visit) the scene waits for the account's real
	// preference, which the appearance boot applies as `data-illustrations` on the root, so a
	// scene is never shown and then removed.
	const known = () => {
		try {
			return JSON.parse(localStorage['lodestar.appearance']).illustrations as boolean;
		} catch {
			const v = document.documentElement.dataset.illustrations;
			return v && v === 'true';
		}
	};
	let on = $state(known());
	$effect(() => {
		if (on !== undefined) return;
		const o = new MutationObserver(() => (on = known()));
		o.observe(document.documentElement, { attributeFilter: ['data-illustrations'] });
		return () => o.disconnect();
	});
	const saveData = (navigator as Navigator & { connection?: { saveData?: boolean } }).connection
		?.saveData;
	const scene = $derived(scenes.find((s) => s.id === id && s.mode === themeMode()));
</script>

{#if on && !saveData && scene}
	<picture class="block overflow-hidden rounded-box bg-base-100">
		<source type="image/avif" srcset={scene.srcset.avif} sizes="160px" />
		<source type="image/webp" srcset={scene.srcset.webp} sizes="160px" />
		<img
			src={scene.srcset.webp.split(' ')[0]}
			alt=""
			width={scene.width}
			height={scene.height}
			loading="lazy"
			decoding="async"
			class="h-(--art-height-small) w-auto large:h-(--art-height-large)"
		/>
	</picture>
{/if}
