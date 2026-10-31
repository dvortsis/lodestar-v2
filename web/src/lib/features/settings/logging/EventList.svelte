<script lang="ts">
	// The event list (§10.10.12): every catalogued event grouped by component, each group
	// collapsible with a count ("Fetch · 4 of 11 on"). A row has a toggle, the plain name, the level
	// (icon + text), the current rate from the counters, badges for the data kinds it can carry and
	// one line of help. Required events show a lock and why; an opt-in's event follows its opt-in.
	// Changes are staged as overrides of the Verbosity preset; Reset to preset drops them all.
	import { getContext } from 'svelte';
	import IconLock from '~icons/ph/lock-simple';
	import IconError from '~icons/ph/x-circle';
	import IconWarn from '~icons/ph/warning';
	import IconInfo from '~icons/ph/info';
	import IconDebug from '~icons/ph/bug';
	import IconTrace from '~icons/ph/dots-three-circle';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import type { WidgetProps } from '../form/widgets';
	import type { CatalogHolder, CatalogKey } from './context';
	import { componentLabel, eventHelp, eventLabel, kindShort } from './event-labels';
	import { levelLabel } from './labels';
	import { eventOn, toggleEvent, type LogEvent } from './rules';

	let { form, key, controlProps }: WidgetProps = $props();
	const holder = getContext<CatalogHolder>('logging-catalog' satisfies CatalogKey);
	const catalog = $derived(holder.catalog);
	const ICONS = {
		error: IconError,
		warn: IconWarn,
		info: IconInfo,
		debug: IconDebug,
		trace: IconTrace
	};

	const f = $derived(form.field(key));
	const overrides = $derived((f.value ?? {}) as Record<string, boolean>);
	const verbosity = $derived(String(form.field('logging.verbosity').value ?? 'normal'));
	const components = $derived(
		(form.specs.some((s) => s.name === 'components')
			? (form.field('logging.components').value ?? {})
			: {}) as Record<string, string>
	);

	// Groups the user opened or closed keep that state when a search is cleared. A group nobody has
	// touched is open while searching. A resync doesn't remount the list, so this survives it.
	const opened = $state<Record<string, boolean>>({});
	let query = $state('');
	let changedOnly = $state(false);
	const matches = (e: LogEvent, q: string) =>
		!q || `${eventLabel(e)} ${eventHelp(e) ?? ''} ${e.id}`.toLocaleLowerCase().includes(q);
	const groups = $derived.by(() => {
		const q = query.trim().toLocaleLowerCase();
		const out: Record<string, { all: LogEvent[]; shown: LogEvent[]; on: number }> = {};
		for (const e of catalog?.catalog?.events ?? []) {
			const g = (out[e.component] ??= { all: [], shown: [], on: 0 });
			g.all.push(e);
			if (eventOn(e, verbosity, components, overrides)) g.on++;
			if (matches(e, q) && (!changedOnly || e.id in overrides)) g.shown.push(e);
		}
		return Object.entries(out).filter(([, g]) => g.shown.length);
	});
	const filtering = $derived(query.trim() !== '' || changedOnly);
	// "Detailed for 30 minutes" from the live log raises every component's level for now, so more
	// events record than the saved settings this list shows.
	const temporary = $derived.by(() => {
		const t = catalog?.catalog?.presets.temporary;
		return t && t.until > (catalog?.now ?? 0) ? t : null;
	});

	function set(e: LogEvent, on: boolean) {
		form.set(key, toggleEvent(e, on, verbosity, components, overrides));
	}
</script>

<div
	class="flex flex-col gap-3"
	role="group"
	aria-labelledby={controlProps['aria-labelledby']}
	aria-describedby={controlProps['aria-describedby']}
>
	<div class="flex flex-wrap items-center gap-3">
		<Input
			type="search"
			dense
			aria-label={m.settings_logging_events_search()}
			placeholder={m.settings_logging_events_search()}
			bind:value={query}
		/>
		<Checkbox label={m.settings_logging_events_changed()} bind:checked={changedOnly} />
		<Button
			variant="secondary"
			disabled={f.locked || !Object.keys(overrides).length}
			onclick={() => form.set(key, {})}>{m.settings_logging_events_reset()}</Button
		>
	</div>
	{#if temporary}
		<p class="text-small text-muted-text" data-testid="log-events-temporary">
			{m.settings_logging_events_temporary()}
		</p>
	{/if}
	{#if catalog?.error && !catalog.catalog}
		<p role="alert" class="text-small text-error">{m.settings_logging_unavailable()}</p>
	{:else if !catalog?.catalog}
		<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
	{:else if !groups.length}
		<p class="text-muted-text">{m.settings_logging_events_none()}</p>
	{/if}
	{#each groups as [component, g] (component)}
		{@const open = opened[component] ?? filtering}
		<details {open} class="border-t border-base-300">
			<summary
				class="ls-focus cursor-pointer py-3 text-label"
				data-testid="log-group-{component}"
				onclick={(ev) => {
					// The state drives `open`, not the native toggle (its event comes later, so a render
					// in between could close the group again).
					ev.preventDefault();
					opened[component] = !open;
				}}
				>{m.settings_logging_events_group({
					component: componentLabel(component),
					on: formatCount(g.on, currentLocale()),
					total: formatCount(g.all.length, currentLocale())
				})}</summary
			>
			<ul class="flex flex-col gap-4 pb-4">
				{#each g.shown as e (e.id)}
					{@const Icon = ICONS[e.level]}
					{@const on = eventOn(e, verbosity, components, overrides)}
					<li class="flex flex-col gap-1" data-testid="log-event-{e.id}">
						<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
							<Toggle
								label={eventLabel(e)}
								checked={on}
								disabled={f.locked || e.required || e.opt_in !== null}
								onchange={(ev) => set(e, ev.currentTarget.checked)}
							/>
							<span class="inline-flex items-center gap-1 text-small text-muted-text">
								<Icon class="size-4" aria-hidden="true" />{levelLabel(e.level)}
							</span>
							<span class="text-small text-muted-text" data-testid="log-rate-{e.id}"
								>{m.settings_logging_events_rate({
									count: formatCount(e.rate_per_hour, currentLocale())
								})}</span
							>
							{#each e.kinds as k (k)}<Badge>{kindShort(k)}</Badge>{/each}
							{#if e.id in overrides}<Badge variant="accent"
									>{m.settings_logging_events_changed_badge()}</Badge
								>{/if}
						</div>
						{#if eventHelp(e)}<p class="text-small text-muted-text">{eventHelp(e)}</p>{/if}
						{#if e.required}
							<p class="inline-flex items-center gap-1 text-small text-muted-text">
								<IconLock class="size-4" aria-hidden="true" />{m.settings_logging_events_required()}
							</p>
						{:else if e.opt_in}
							<p class="text-small text-muted-text">{m.settings_logging_events_opt_in()}</p>
						{/if}
					</li>
				{/each}
			</ul>
		</details>
	{/each}
</div>
