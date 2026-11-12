<script lang="ts">
	// Settings → Outputs (§10.10.8, OUT-2): what other apps can take from this index. Torznab for
	// the *arr apps (TZ-0), and the bitmagnet export (OUT-0). Each section loads and fails on its own.
	import { m } from '$lib/paraglide/messages.js';
	import Bitmagnet from './Bitmagnet.svelte';
	import Torznab from './Torznab.svelte';
</script>

<div class="flex flex-col gap-10" data-testid="outputs-screen">
	<section class="flex flex-col gap-4" aria-labelledby="outputs-torznab">
		<h2 id="outputs-torznab" class="text-small font-semibold">
			{m.settings_outputs_torznab_heading()}
		</h2>
		<Torznab />
	</section>
	<section class="flex flex-col gap-4" aria-labelledby="outputs-bitmagnet">
		<h2 id="outputs-bitmagnet" class="text-small font-semibold">
			{m.settings_outputs_bitmagnet_heading()}
		</h2>
		<Bitmagnet />
	</section>
</div>
