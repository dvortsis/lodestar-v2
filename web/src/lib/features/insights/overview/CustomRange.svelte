<script lang="ts">
	// The custom range's two UTC-day inputs. Loaded on demand: most visits never open it.
	import { untrack } from 'svelte';
	import IconError from '~icons/ph/x-circle';
	import Button from '$lib/ui/controls/Button.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { customFromDays } from './query';

	interface Props {
		from: string;
		to: string;
		/** A server `400` for the range, shown on its field. */
		fieldError: { field: string; message: string } | undefined;
		onapply: (from: number, to: number) => void;
		oncancel: () => void;
	}
	let { from, to, fieldError, onapply, oncancel }: Props = $props();

	let fromDay = $state(untrack(() => from));
	let toDay = $state(untrack(() => to));
	let localError = $state<'from' | 'to' | undefined>();

	function apply(event: SubmitEvent) {
		event.preventDefault();
		const r = customFromDays(fromDay, toDay);
		if (!r) {
			localError = fromDay ? 'to' : 'from';
			return;
		}
		localError = undefined;
		onapply(r.from, r.to);
	}
	const errorFor = (field: 'from' | 'to') =>
		fieldError?.field === field
			? fieldError.message
			: localError === field
				? m.insights_overview_custom_invalid()
				: undefined;
</script>

<form class="ins-custom" onsubmit={apply} data-testid="insights-custom">
	{#each [['from', m.insights_overview_custom_from()], ['to', m.insights_overview_custom_to()]] as [field, text] (field)}
		{@const err = errorFor(field as 'from' | 'to')}
		<div class="ins-field">
			<label for="ins-custom-{field}">{text}</label>
			<Input
				id="ins-custom-{field}"
				type="date"
				dense
				value={field === 'from' ? fromDay : toDay}
				oninput={(e) => {
					const v = (e.currentTarget as HTMLInputElement).value;
					if (field === 'from') fromDay = v;
					else toDay = v;
				}}
				aria-invalid={err ? true : undefined}
				aria-describedby={err ? `ins-custom-${field}-error` : undefined}
				data-testid="insights-custom-{field}"
			/>
			{#if err}
				<span
					id="ins-custom-{field}-error"
					class="ins-error"
					role="alert"
					data-testid="insights-custom-{field}-error"
					><IconError class="ins-icon-s" aria-hidden="true" />{err}</span
				>
			{/if}
		</div>
	{/each}
	<div class="ins-field">
		<span aria-hidden="true">&nbsp;</span>
		<div class="ins-row">
			<Button type="submit" variant="primary" dense>{m.insights_overview_custom_apply()}</Button>
			<Button variant="quiet" dense onclick={oncancel}>{m.insights_overview_custom_cancel()}</Button
			>
		</div>
	</div>
</form>
