<script lang="ts">
	// Typed From and To dates beside the brush (UI review U15). Applies when both texts are valid
	// and in order; otherwise says what to change and leaves the clause alone.
	import { m } from '$lib/paraglide/messages.js';
	import { dayText, rangeOfTyped } from './dates';

	interface Props {
		/** The clause's inclusive bounds in UTC seconds (null: none). */
		range: { lo: number | null; hi: number | null } | null;
		onrange: (range: { lo: number | null; hi: number | null } | null) => void;
	}
	let { range, onrange }: Props = $props();

	// What the person has typed; it follows the clause when the clause changes elsewhere.
	let from = $state('');
	let to = $state('');
	$effect(() => {
		from = dayText(range?.lo ?? null);
		to = dayText(range?.hi ?? null);
	});
	let error = $state<string | null>(null);

	function apply() {
		const r = rangeOfTyped(from, to);
		if (!r.ok) {
			error = r.reason === 'order' ? m.search_facets_dates_order() : m.search_facets_dates_bad();
			return;
		}
		error = null;
		onrange(r.range);
	}
</script>

<div class="fr-dates" data-testid="facet-discovered-dates">
	<label class="fr-date">
		<span>{m.search_facets_date_from()}</span>
		<input
			type="date"
			class="fr-date-input"
			bind:value={from}
			onchange={apply}
			aria-invalid={error ? 'true' : undefined}
			aria-describedby={error ? 'fr-dates-error' : undefined}
			data-testid="facet-discovered-from"
		/>
	</label>
	<label class="fr-date">
		<span>{m.search_facets_date_to()}</span>
		<input
			type="date"
			class="fr-date-input"
			bind:value={to}
			onchange={apply}
			aria-invalid={error ? 'true' : undefined}
			aria-describedby={error ? 'fr-dates-error' : undefined}
			data-testid="facet-discovered-to"
		/>
	</label>
</div>
{#if error}<p
		class="fr-muted fr-error"
		id="fr-dates-error"
		role="alert"
		data-testid="facet-discovered-dates-error"
	>
		{error}
	</p>{/if}
