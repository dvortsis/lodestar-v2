<script lang="ts">
	// The server's plain-language effect warning (§20.11), in its own labelled region. Rendered as
	// text only: it's built from names and queries the user (or a list) supplied.
	import IconWarning from '~icons/ph/warning';
	import { m } from '$lib/paraglide/messages.js';

	interface Props {
		text: string;
		/** The id the password field points `aria-describedby` at. */
		id?: string;
		/** Adds "Enter your password to apply." under the warning. */
		password?: boolean;
	}

	let { text, id, password = false }: Props = $props();
</script>

<section
	class="flex items-start gap-3 rounded-box border border-warning bg-base-100 p-3 text-body"
	aria-label={m.filters_flows_warning_label()}
	data-testid="effect-warning"
>
	<IconWarning class="mt-0.5 size-5 shrink-0 text-warning" aria-hidden="true" />
	<div {id} class="flex min-w-0 flex-col gap-2">
		<span class="sr-only">{m.filters_flows_warning_label()}:</span>
		<p data-testid="effect-warning-text">{text}</p>
		{#if password}<p class="font-medium">{m.filters_flows_password_prompt()}</p>{/if}
	</div>
</section>
