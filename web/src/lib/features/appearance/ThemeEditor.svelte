<script lang="ts">
	// The custom theme editor (§10.18): the five base colours and six categorical colours of each
	// mode, a live contrast report from the build's own theme compiler, JSON import and export, and
	// "Use anyway" for a theme that fails a gate. Custom themes live in this browser (decision file).
	// Loaded when "Create a custom theme" is pressed, with the compiler.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import SegmentedControl from '$lib/ui/controls/SegmentedControl.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Textarea from '$lib/ui/fields/Textarea.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import {
		COLOUR_FIELDS,
		HEX,
		START,
		compile,
		paletteOf,
		parseImport,
		slug,
		type Family,
		type Palette
	} from './editor';
	import type { ThemeEntry, ThemesFile } from './themes';
	import './editor.css';

	interface Props {
		file: ThemesFile;
		custom: ThemeEntry[];
		write: (list: ThemeEntry[]) => void;
		/** The id of a custom theme being edited; none creates one. */
		editing?: string;
		onsave: (id: string) => void;
		onclose: () => void;
	}
	let { file, custom, write, editing, onsave, onclose }: Props = $props();

	const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
	/** Everything the props decide at open: the starting points and, when editing, the saved palettes. */
	function seed() {
		const bases: Family[] = [START];
		for (const t of file.themes) {
			const id = t.id.replace(/-(light|dark)$/, '');
			if (t.mode === 'light' && !bases.some((b) => b.id === id)) {
				const dark = file.themes.find((x) => x.id === `${id}-dark`);
				if (dark)
					bases.push({
						id,
						name: t.family,
						group: 'original',
						light: paletteOf(file, t),
						dark: paletteOf(file, dark)
					});
			}
		}
		const palette = (md: string) =>
			(custom.find((t) => t.id === `${editing}-${md}`)?.palette as Palette | undefined) ??
			undefined;
		return {
			bases,
			name: custom.find((t) => t.id === `${editing}-light`)?.family ?? '',
			taken: custom.map((t) => t.id.replace(/-(light|dark)$/, '')),
			light: clone(palette('light') ?? START.light),
			dark: clone(palette('dark') ?? START.dark)
		};
	}
	const init = seed();
	const bases = init.bases;
	let name = $state(init.name);
	let light = $state<Palette>(init.light);
	let dark = $state<Palette>(init.dark);
	let mode = $state<'light' | 'dark'>('light');
	let base = $state('riverstone');
	let text = $state('');
	let notes = $state<string[]>([]);
	let weak = $state(false);

	const palette = $derived(mode === 'light' ? light : dark);
	const valid = (p: Palette) =>
		Array.isArray(p.categorical) &&
		[...COLOUR_FIELDS.map((k) => p[k]), ...p.categorical].every((c) => HEX.test(c));
	const family = $derived<Family>({
		id: 'custom',
		name: name || 'Custom',
		group: 'original',
		light,
		dark
	});
	const both = $derived(
		valid(light) && valid(dark)
			? { light: compile(family, 'light'), dark: compile(family, 'dark') }
			: null
	);
	const current = $derived(both?.[mode]);
	const failed = $derived(both ? [...both.light.failed, ...both.dark.failed] : []);

	function start(id: string) {
		base = id;
		const b = bases.find((x) => x.id === id);
		if (!b) return;
		light = clone(b.light);
		dark = clone(b.dark);
	}

	function set(key: (typeof COLOUR_FIELDS)[number] | number, value: string) {
		const p = mode === 'light' ? light : dark;
		if (typeof key === 'number') p.categorical[key] = value;
		else p[key] = value;
		weak = false;
	}

	function save() {
		if (!both || !name.trim()) return;
		if (failed.length && !weak) {
			weak = true;
			return;
		}
		const id = editing ?? slug(name, init.taken);
		const entries = (['light', 'dark'] as const).map((md) => ({
			...both[md].entry,
			id: `${id}-${md}`,
			name: `${name.trim()} ${md === 'light' ? 'Light' : 'Dark'}`,
			family: name.trim(),
			palette: md === 'light' ? clone(light) : clone(dark),
			keys: file.keys,
			...(failed.length ? { warn: true } : {})
		}));
		write([...custom.filter((t) => t.id !== `${id}-light` && t.id !== `${id}-dark`), ...entries]);
		onsave(id);
	}

	function exportJson() {
		text = JSON.stringify(
			{ id: 'custom', name: name || 'Custom', group: 'original', light, dark },
			null,
			2
		);
		notes = [];
	}

	function importJson() {
		const res = parseImport(text);
		if (!res.family) {
			notes = [m.appearance_editor_import_failed()];
			return;
		}
		name = res.family.name;
		light = clone(res.family.light);
		dark = clone(res.family.dark);
		notes = [];
		weak = false;
	}

	const modes = $derived([
		{ value: 'light', label: m.appearance_editor_light() },
		{ value: 'dark', label: m.appearance_editor_dark() }
	]);
	const labels = $derived({
		background: m.appearance_editor_background(),
		surface: m.appearance_editor_surface(),
		text: m.appearance_editor_text(),
		primary: m.appearance_editor_primary(),
		accent: m.appearance_editor_accent()
	});
</script>

{#snippet colour(label: string, value: string, change: (v: string) => void)}
	<div class="ed-colour">
		<input
			type="color"
			class="ed-swatch"
			aria-label={label}
			value={HEX.test(value) ? value.toLowerCase() : undefined}
			oninput={(e) => change(e.currentTarget.value)}
		/>
		<span class="ed-name">{label}</span>
		<Input
			dense
			mono
			aria-label={m.appearance_editor_hex({ name: label })}
			aria-invalid={!HEX.test(value)}
			{value}
			oninput={(e) => change(e.currentTarget.value)}
		/>
	</div>
{/snippet}

<section class="ed-root" aria-labelledby="ed-title" data-testid="theme-editor">
	<h3 id="ed-title" class="ed-title">{m.appearance_editor_title()}</h3>
	<div class="ed-field">
		<label for="ed-name" class="ed-label">{m.appearance_editor_name()}</label>
		<Input id="ed-name" bind:value={name} maxlength={40} />
	</div>
	{#if !editing}
		<div class="ed-field">
			<label for="ed-base" class="ed-label">{m.appearance_editor_start()}</label>
			<Select
				id="ed-base"
				value={base}
				options={bases.map((b) => ({ value: b.id, label: b.name }))}
				onchange={(e) => start(e.currentTarget.value)}
			/>
		</div>
	{/if}
	<SegmentedControl
		label={m.appearance_editor_palette()}
		options={modes}
		value={mode}
		onchange={(v) => (mode = v as 'light' | 'dark')}
	/>
	<div class="ed-grid">
		{#each COLOUR_FIELDS as key (key)}
			{@render colour(labels[key], palette[key], (v) => set(key, v))}
		{/each}
		{#each palette.categorical as value, i (i)}
			{@render colour(m.appearance_editor_category({ n: i + 1 }), value, (v) => set(i, v))}
		{/each}
	</div>

	<h4 class="ed-subtitle">{m.appearance_editor_report()}</h4>
	{#if !current}
		<Banner kind="warning">{m.appearance_editor_invalid()}</Banner>
	{:else}
		<p class="ed-text" role="status">
			{failed.length
				? m.appearance_editor_failing({ count: failed.length })
				: m.appearance_editor_passing({ level: current.report.contrast })}
		</p>
		<table class="ed-table">
			<caption class="ed-caption">{m.appearance_editor_checks()}</caption>
			<thead>
				<tr>
					<th scope="col">{m.appearance_editor_col_check()}</th>
					<th scope="col">{m.appearance_editor_col_value()}</th>
					<th scope="col">{m.appearance_editor_col_result()}</th>
				</tr>
			</thead>
			<tbody>
				{#each current.report.checks as c (c.label)}
					<tr>
						<th scope="row">{c.label}</th>
						<td>{c.value.toFixed(2)} / {c.min}</td>
						<td>
							<Badge variant={c.pass ? 'success' : c.severity === 'fail' ? 'error' : 'warning'}>
								{c.pass
									? m.appearance_editor_pass()
									: c.severity === 'fail'
										? m.appearance_editor_fail()
										: m.appearance_editor_warn()}
							</Badge>
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
		<p class="ed-text">
			{m.appearance_editor_cvd({
				normal: current.report.normalDeltaE.toFixed(1),
				deuteranopia: Math.min(
					...current.report.cvd
						.filter((r) => r.kind === 'deuteranopia')
						.map((r) => Math.min(r.machado, r.vienot))
				).toFixed(1),
				protanopia: Math.min(
					...current.report.cvd
						.filter((r) => r.kind === 'protanopia')
						.map((r) => Math.min(r.machado, r.vienot))
				).toFixed(1)
			})}
		</p>
	{/if}

	{#if weak}
		<Banner kind="warning">{m.appearance_editor_weak({ count: failed.length })}</Banner>
	{/if}
	<div class="ed-actions">
		<Button variant="primary" disabled={!both || !name.trim()} onclick={save}>
			{weak ? m.appearance_editor_use_anyway() : m.appearance_editor_save()}
		</Button>
		<Button onclick={onclose}>{m.appearance_editor_close()}</Button>
	</div>

	<h4 class="ed-subtitle">{m.appearance_editor_json()}</h4>
	<p class="ed-text">{m.appearance_editor_json_help()}</p>
	<Textarea aria-label={m.appearance_editor_json()} rows={8} bind:value={text} />
	{#each notes as note (note)}<p class="ed-text" role="alert">{note}</p>{/each}
	<div class="ed-actions">
		<Button onclick={exportJson}>{m.appearance_editor_export()}</Button>
		<Button disabled={!text.trim()} onclick={importJson}>{m.appearance_editor_import()}</Button>
	</div>
</section>
