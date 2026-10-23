<script lang="ts">
	// A form's fields as groups (§10.10.2): each with a heading, its essentials as `SettingField`s,
	// then "Advanced (n)", a closed disclosure remembered per screen. Generated screens render
	// this; hand-built screens embed it where they can (§10.10.8), inside their own
	// `<SettingsForm {form}>`, so the field behaves the same everywhere. A screen never builds a
	// field by hand. It also answers deep links (`#crawler.intensity`) once the form is ready.
	//
	// `onReveal` for `<SettingsForm>` (the error summary's links): `onReveal={(k) => groups?.reveal(k)}`
	// with `bind:this={groups}`.
	import { page } from '$app/state';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { tick, untrack, type Snippet } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import {
		groupFields,
		settingText,
		type FieldKey,
		type FieldSpec,
		type SettingsForm
	} from '../form';
	import SettingField from '../form/SettingField.svelte';
	import { readOpen, writeOpen } from './advanced';
	import { fieldFromHash, showField } from './deeplink';

	interface Props {
		form: SettingsForm;
		/** The fields to show; the form's own by default. */
		specs?: FieldSpec[];
		/** The screen's id: where the Advanced disclosures are remembered. */
		screen: string;
		/** Only fields that differ from their default (Show changed only). */
		changedOnly?: boolean;
		/** Appended to a group (the Network screen's reachability panel). */
		after?: Snippet<[group: string]>;
	}

	let { form, specs, screen, changedOnly = false, after }: Props = $props();

	const shown = $derived(specs ?? form.specs);
	const groups = $derived(
		groupFields(shown)
			.map((g) => {
				if (!changedOnly) return g;
				const changed = (s: FieldSpec) => form.field(s.key).changed;
				return {
					...g,
					essentials: g.essentials.filter(changed),
					advanced: g.advanced.filter(changed)
				};
			})
			.filter((g) => g.essentials.length + g.advanced.length > 0)
	);

	// What the user opened (kept in storage) and what a reveal opened (a deep link, search, the
	// error summary, Show changed only: not remembered).
	// The screen id never changes while mounted.
	const remembered = new SvelteSet<string>(readOpen(untrack(() => screen)));
	const revealed = new SvelteSet<string>();
	const isOpen = (group: string) => changedOnly || remembered.has(group) || revealed.has(group);

	function toggled(group: string, open: boolean) {
		if (open) {
			if (revealed.has(group) || changedOnly) return;
			remembered.add(group);
		} else {
			remembered.delete(group);
			revealed.delete(group);
		}
		writeOpen(screen, remembered);
	}

	/** Opens the Advanced disclosure holding a field, without remembering it. Resolves once it's in the DOM. */
	export async function reveal(key: FieldKey): Promise<void> {
		const group = groupFields(shown).find((g) => g.advanced.some((s) => s.key === key));
		if (group && !isOpen(group.group)) {
			revealed.add(group.group);
			await tick();
		}
	}

	const keys = $derived(shown.map((s) => s.key));
	const hash = $derived(page.url.hash);
	$effect(() => {
		const key = fieldFromHash(hash, keys);
		if (!key || form.status === 'loading' || form.status === 'load-error') return;
		void reveal(key).then(() => showField(key));
	});
</script>

{#each groups as g (g.group)}
	<section class="flex flex-col gap-4" aria-labelledby="group-{screen}-{g.group}">
		<h2 id="group-{screen}-{g.group}" class="text-title font-semibold">
			{settingText(g.labelKey) ?? g.group}
		</h2>
		{#each g.essentials as s (s.key)}
			<SettingField {form} key={s.key} />
		{/each}
		{#if g.advanced.length}
			<details
				class="settings-advanced"
				open={isOpen(g.group)}
				ontoggle={(e) => toggled(g.group, e.currentTarget.open)}
				data-testid="advanced-{g.group}"
			>
				<summary class="settings-advanced-summary"
					>{m.settings_ui_advanced({
						count: formatCount(g.advanced.length, currentLocale())
					})}</summary
				>
				<div class="settings-advanced-body">
					{#each g.advanced as s (s.key)}
						<SettingField {form} key={s.key} />
					{/each}
				</div>
			</details>
		{/if}
		{@render after?.(g.group)}
	</section>
{/each}

<style>
	.settings-advanced-summary {
		display: flex;
		align-items: center;
		min-height: var(--control-height);
		cursor: pointer;
	}
	@media (pointer: coarse) {
		.settings-advanced-summary {
			min-height: var(--target-min);
		}
	}
	.settings-advanced-summary:focus-visible {
		outline: var(--focus-ring-width) solid var(--color-primary);
		outline-offset: var(--focus-ring-offset);
	}
	.settings-advanced-body {
		display: flex;
		flex-direction: column;
		gap: calc(var(--spacing) * 4);
		margin-top: calc(var(--spacing) * 2);
	}
	/* A deep link's target: fades in, then out; under reduced motion it holds a static outline. */
	:global([data-field][data-highlight]) {
		outline: var(--focus-ring-width) solid var(--color-primary);
		outline-offset: var(--focus-ring-offset);
		animation: settings-highlight 2s var(--ease-out);
	}
	@keyframes settings-highlight {
		from {
			outline-color: transparent;
		}
		20% {
			outline-color: var(--color-primary);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		:global([data-field][data-highlight]) {
			animation: none;
		}
	}
</style>
