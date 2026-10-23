<script lang="ts">
	// A state illustration (§10.19): decorative (`alt=""`), the variant follows the active theme's
	// mode, a 12 px frame, no animation, lazy. The parent puts the copy and the next step first in
	// the DOM. The slot in StateScreen hides it under 600 px of viewport height.
	import { scenes, type SceneId } from '$lib/brand/scenes.generated';
	import { themeMode } from './theme-mode';

	let { id }: { id: SceneId } = $props();

	const scene = $derived(scenes.find((s) => s.id === id && s.mode === themeMode()));
</script>

{#if scene}
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
