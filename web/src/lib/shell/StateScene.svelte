<script lang="ts">
	// A state illustration (§10.19): decorative (`alt=""`), the variant follows the active theme's
	// mode, a 12 px frame, no animation, lazy. The parent puts the copy and the next step first in
	// the DOM. The slot in StateScreen hides it under 600 px of viewport height.
	import { scenes, type SceneId } from '$lib/brand/scenes.generated';
	import { themeMode } from './theme-mode';

	let { id }: { id: SceneId } = $props();

	// Off when the account turned them off or the browser's data saver is on. Not rendered, so no
	// image is fetched. The per-browser cache (written with `data-illustrations` on every change)
	// holds only strings and `illustrations`, the one boolean, so "false" in it means off; it is
	// read when a scene is built, so a change made on the Appearance screen applies from then on.
	// The pre-paint script sets the theme only and the appearance boot waits for first interaction,
	// so the attribute on <html> is not there yet on a first view.
	let shown = true;
	try {
		shown = !(
			(navigator as Navigator & { connection?: { saveData?: boolean } }).connection?.saveData ||
			localStorage['lodestar.appearance'].includes('false')
		);
	} catch {
		/* no cache yet, or storage blocked: show them */
	}
	const scene = $derived(scenes.find((s) => s.id === id && s.mode === themeMode()));
</script>

{#if shown && scene}
	<picture class="block overflow-hidden rounded-box bg-base-100">
		<source type="image/avif" srcset={scene.srcset.avif} sizes="160px" />
		<source type="image/webp" srcset={scene.srcset.webp} sizes="160px" />
		<img
			src={scene.srcset.webp.split(' ')[0]}
			alt=""
			width={scene.width}
			height={scene.height}
			loading="lazy"
			class="h-(--art-height-small) w-auto large:h-(--art-height-large)"
		/>
	</picture>
{/if}
