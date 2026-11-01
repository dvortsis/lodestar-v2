<script lang="ts">
	// A state illustration (§10.19): decorative (`alt=""`), the variant follows the active theme's
	// mode, a 12 px frame, no animation, lazy. The parent puts the copy and the next step first in
	// the DOM. The slot in StateScreen hides it under 600 px of viewport height.
	import { scenes, type SceneId } from '$lib/brand/scenes.generated';
	import { themeMode } from './theme-mode';

	let { id }: { id: SceneId } = $props();

	// Off when the account turned them off or the browser's data saver is on. Not rendered, so no
	// image is fetched. Read from the per-browser cache (written by `applyLook` on every change)
	// when a scene is built: the pre-paint script sets the theme only and the appearance boot waits
	// for first interaction, so the cache is what a first view has.
	// svelte-ignore non_reactive_update
	let shown = true;
	try {
		shown = !(
			(navigator as Navigator & { connection?: { saveData?: boolean } }).connection?.saveData ||
			!JSON.parse(localStorage['lodestar.appearance']).illustrations
		);
	} catch {
		/* no cache yet, or storage blocked: show them */
	}
	const scene = $derived(scenes.find((s) => s.id === id && s.mode === themeMode()));
</script>

{#if shown && scene}
	<picture data-scene class="block overflow-hidden rounded-box bg-base-100">
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
