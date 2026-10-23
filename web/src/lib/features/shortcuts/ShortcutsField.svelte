<script lang="ts">
	// The `x-widget: shortcuts` editor (preferences.shortcut_keys): one row per remappable action.
	// Controlled: `value` is the remaps (`{action: key}`), `onchange` gets the next map, nothing is
	// saved here. "Change" takes the next key press; a key another action has asks for a Swap.
	import IconError from '~icons/ph/x-circle';
	import IconWarning from '~icons/ph/warning';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { ACTION_IDS, actionTitle, defaultKey, type ActionId } from './catalog';
	import { tick } from 'svelte';
	import {
		capture,
		clash,
		collisions,
		effective,
		swap,
		withKey,
		type KeyMap,
		type Refusal
	} from './capture';
	import Keycap from './Keycap.svelte';
	import { keyNameFor } from './keyname';
	import './shortcuts.css';

	interface Props {
		value: KeyMap;
		onchange: (next: KeyMap) => void;
		/** Messages by action id; `''` is the field as a whole. */
		errors?: Record<string, string>;
		disabled?: boolean;
	}
	let { value, onchange, errors = {}, disabled = false }: Props = $props();

	let capturing = $state<ActionId>();
	let pending = $state<{ id: ActionId; key: string; other: ActionId }>();
	let refusal = $state<{ id: ActionId; why: Refusal }>();
	let live = $state('');

	const REFUSALS: Record<Refusal, () => string> = {
		shift: () => m.shortcuts_err_shift(),
		modifier: () => m.shortcuts_err_modifier(),
		blank: () => m.shortcuts_err_not_visible(),
		plus: () => m.shortcuts_err_plus()
	};
	const remapped = $derived(ACTION_IDS.some((id) => id in value));

	function propose(id: ActionId, key: string) {
		const other = clash(value, id, key);
		if (other) {
			pending = { id, key, other };
			live = '';
			return;
		}
		pending = undefined;
		refusal = undefined;
		onchange(withKey(value, id, key));
		live = m.shortcuts_capture_set({ action: actionTitle(id), key: keyNameFor(key) });
	}

	function stop() {
		capturing = undefined;
		refusal = undefined;
	}

	/** A button that removes itself leaves focus nowhere: put it on the row's Change button. */
	function focusChange(id: ActionId) {
		void tick().then(() =>
			document.querySelector<HTMLElement>(`[data-testid="shortcut-change-${id}"]`)?.focus()
		);
	}

	function onkey(event: KeyboardEvent) {
		const id = capturing;
		if (!id || event.repeat) return;
		const got = capture(event);
		// Tab cancels and still moves focus, so it is a way out. Any other key is a choice, not a
		// command: taken before the registry's own listener sees it.
		if (event.key !== 'Tab') {
			event.preventDefault();
			event.stopPropagation();
		}
		if ('ignore' in got) return;
		if ('cancel' in got) {
			stop();
			live = m.shortcuts_capture_cancelled();
			return;
		}
		if ('refuse' in got) {
			refusal = { id, why: got.refuse };
			live = '';
			return;
		}
		stop();
		propose(id, got.key);
	}

	$effect(() => {
		if (!capturing) return;
		window.addEventListener('keydown', onkey, true);
		return () => window.removeEventListener('keydown', onkey, true);
	});

	function begin(id: ActionId) {
		refusal = undefined;
		pending = undefined;
		capturing = id;
		live = m.shortcuts_capture_prompt();
	}

	function reset(id: ActionId) {
		pending = undefined;
		refusal = undefined;
		propose(id, defaultKey(id));
		focusChange(id);
	}
</script>

<div class="sc-field" data-testid="shortcuts-field">
	<p class="sr-only" role="status" aria-live="polite">{live}</p>
	<ul class="sc-list">
		{#each ACTION_IDS as id (id)}
			{@const key = effective(value, id)}
			{@const title = actionTitle(id)}
			{@const clashing = pending?.id === id ? pending : undefined}
			{@const msg = (refusal?.id === id && REFUSALS[refusal.why]()) || errors[id]}
			{@const others = collisions(key)}
			<li class="sc-row" data-testid="shortcut-row-{id}">
				<span class="sc-name">{title}</span>
				<span class="sc-key" data-testid="shortcut-key-{id}">
					{#if capturing === id}
						<span class="sc-prompt">{m.shortcuts_capture_prompt()}</span>
					{:else}
						<Keycap combo={key} />
					{/if}
				</span>
				<span class="sc-actions">
					<Button
						dense
						{disabled}
						aria-pressed={capturing === id}
						aria-label={m.shortcuts_change_label({ action: title })}
						data-testid="shortcut-change-{id}"
						onclick={() => (capturing === id ? stop() : begin(id))}
						onblur={() => capturing === id && stop()}
					>
						{m.shortcuts_change()}
					</Button>
					{#if id in value}
						<Button
							dense
							variant="quiet"
							{disabled}
							aria-label={m.shortcuts_reset_label({ action: title })}
							onclick={() => reset(id)}
						>
							{m.shortcuts_reset()}
						</Button>
					{/if}
				</span>
				{#if clashing}
					<p class="sc-note sc-bad" role="alert">
						<IconError aria-hidden="true" />
						<span>{m.shortcuts_conflict_used({ action: actionTitle(clashing.other) })}</span>
						<Button
							dense
							{disabled}
							onclick={() => {
								onchange(swap(value, id, clashing.other, clashing.key));
								pending = undefined;
								focusChange(id);
							}}>{m.shortcuts_conflict_swap()}</Button
						>
						<Button
							dense
							variant="quiet"
							{disabled}
							onclick={() => {
								pending = undefined;
								focusChange(id);
							}}
						>
							{m.shortcuts_conflict_cancel()}
						</Button>
					</p>
				{/if}
				{#if msg}
					<p class="sc-note sc-bad" role="alert">
						<IconError aria-hidden="true" /><span>{msg}</span>
					</p>
				{/if}
				{#each others as c (c.id)}
					<p class="sc-note sc-warn">
						<IconWarning aria-hidden="true" />
						<span>{m.shortcuts_conflict_command({ key: keyNameFor(key), command: c.title })}</span>
					</p>
				{/each}
			</li>
		{/each}
	</ul>
	{#if errors['']}
		<p class="sc-note sc-bad"><IconError aria-hidden="true" /><span>{errors['']}</span></p>
	{/if}
	{#if remapped}
		<div class="sc-all">
			<Button
				variant="quiet"
				{disabled}
				onclick={() => {
					pending = undefined;
					onchange({});
					focusChange(ACTION_IDS[0]);
				}}
			>
				{m.shortcuts_reset_all()}
			</Button>
		</div>
	{/if}
</div>
