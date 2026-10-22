<script lang="ts">
	import { GLYPHS, type GlyphName } from './glyphs';

	let { name, class: className = 'size-(--icon-size-nav)' }: { name: GlyphName; class?: string } =
		$props();
</script>

<svg viewBox="0 0 256 256" fill="currentColor" class={className} aria-hidden="true"
	><path d={GLYPHS[name]} /></svg
>
