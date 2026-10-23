<script lang="ts">
	// §10.5.2: the composition, then the file tree. One lazy chunk, loaded when the section scrolls
	// near (the virtual list under the tree is the heavy part).
	import Composition from './composition/Composition.svelte';
	import FileTree from './files/FileTree.svelte';
	import type { SearchHit, TorrentDetail } from './cache';

	interface Props {
		detail: TorrentDetail;
		hit: SearchHit | null;
	}
	let { detail, hit }: Props = $props();
</script>

<Composition composition={hit?.composition} />
<FileTree {detail} matched={hit?.matched_files ?? []} />
