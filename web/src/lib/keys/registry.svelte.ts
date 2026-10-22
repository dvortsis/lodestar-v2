// The keyboard registry and command list (architecture §10.16). Features register their own
// commands and shortcuts from their own files; the shell owns the one `keydown` listener and the
// command palette, and the shortcut sheet (S-keys) reads `list()`.
//
// - `register(command)` returns the unregister function; call it from an `$effect` or `onMount`
//   cleanup. A key comes from `command.keys`, else from `DEFAULT_KEYS[command.id]`, unless
//   `remap` replaced it.
// - Scopes: a command is `global` unless it names a scope. `pushScope(name)` activates one and
//   returns the pop function; the most recently pushed scope with a match wins over the ones
//   below it and over `global`. `{ exclusive: true }` (a modal surface) mutes everything below.
// - Conflicts: two commands claiming the same combo in the same scope are reported by `conflicts()`
//   (and to `onConflict`); neither fires until one is moved, so a clash never runs the wrong one.
// - Typing is never hijacked: in a text field, select or contenteditable only a command with
//   `allowInInput` fires, and only for a combo with Ctrl/Alt/Meta or Escape. Single-key shortcuts
//   also stay quiet inside a dialog or menu, and can be turned off with `setSingleKeys(false)`
//   (WCAG 2.1.4); `remap(id, combos)` rebinds one command.
import { DEFAULT_KEYS } from './defaults';
import {
	comboFromEvent,
	hasCommandModifier,
	isMacPlatform,
	isSingleKey,
	normalizeCombo
} from './combo';

export interface Command {
	/** Stable id (`search.focus`); ids in `DEFAULT_KEYS` get their default key. */
	id: string;
	/** Shown in the palette and the shortcut sheet. Translated by the registering feature. */
	title: string | (() => string);
	/** Palette heading and shortcut-sheet group. */
	section?: string | (() => string);
	/** Combos such as `mod+k`, `/`, `Escape`. Overrides the default map. */
	keys?: readonly string[];
	/** `global` (default) or a name activated with `pushScope`. */
	scope?: string;
	/** Fires while typing in a field (still needs a modifier or Escape). */
	allowInInput?: boolean;
	/** Extra words the palette matches. */
	keywords?: readonly string[];
	/** Hide from the palette (shortcut-only commands). Default: listed. */
	palette?: boolean;
	/** False hides the command from the palette and mutes its keys. */
	when?: () => boolean;
	run: () => void;
}

export interface CommandInfo {
	id: string;
	title: string;
	section: string;
	scope: string;
	/** Effective combos, canonical form for this platform. */
	keys: string[];
	keywords: readonly string[];
	palette: boolean;
	active: boolean;
}

export interface Conflict {
	combo: string;
	scope: string;
	ids: string[];
}

export interface KeysOptions {
	mac?: boolean;
	onConflict?: (conflict: Conflict) => void;
}

const resolve = (v: string | (() => string) | undefined) =>
	typeof v === 'function' ? v() : (v ?? '');
const GLOBAL = 'global';
const TEXT_INPUT =
	/^(text|search|email|url|tel|password|number|date|datetime-local|month|week|time)?$/;

/** True when typing would land in the event target. */
export function isEditable(target: EventTarget | null): boolean {
	if (!(target instanceof HTMLElement)) return false;
	if (target.isContentEditable) return true;
	if (target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) return true;
	if (target instanceof HTMLInputElement) return TEXT_INPUT.test(target.type);
	return /^(textbox|searchbox|combobox)$/.test(target.getAttribute('role') ?? '');
}

const inOverlay = (target: EventTarget | null) =>
	target instanceof Element &&
	!!target.closest('[role="dialog"], [role="alertdialog"], [role="menu"]');

export function createKeys(options: KeysOptions = {}) {
	const mac = options.mac ?? isMacPlatform();
	const onConflict =
		options.onConflict ??
		((c: Conflict) =>
			console.error(`Key conflict on "${c.combo}" (${c.scope}): ${c.ids.join(', ')}`));

	// Plain collections: only `version` is reactive, bumped on every change.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const commands = new Map<string, Command>();
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const overrides = new Map<string, string[]>();
	const scopes: { name: string; exclusive: boolean }[] = [];
	let version = $state(0);
	let singleKeys = $state(true);
	const bump = () => void version++;

	const keysOf = (c: Command): string[] => {
		const spec =
			overrides.get(c.id) ??
			c.keys ??
			(DEFAULT_KEYS as Record<string, readonly string[]>)[c.id] ??
			[];
		return spec.map((k) => normalizeCombo(k, mac));
	};

	function findConflicts(): Conflict[] {
		// eslint-disable-next-line svelte/prefer-svelte-reactivity
		const seen = new Map<string, string[]>();
		for (const c of commands.values()) {
			// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a local lookup, not state
			for (const combo of new Set(keysOf(c))) {
				const k = `${c.scope ?? GLOBAL}\u0000${combo}`;
				seen.set(k, [...(seen.get(k) ?? []), c.id]);
			}
		}
		return [...seen]
			.filter(([, ids]) => ids.length > 1)
			.map(([k, ids]) => {
				const [scope, combo] = k.split('\u0000') as [string, string];
				return { scope, combo, ids };
			});
	}

	function register(command: Command): () => void {
		if (commands.has(command.id)) throw new Error(`command "${command.id}" is already registered`);
		// Throws on a malformed combo, at registration rather than at the first key press.
		keysOf(command);
		commands.set(command.id, command);
		bump();
		for (const c of findConflicts()) if (c.ids.includes(command.id)) onConflict(c);
		return () => {
			if (commands.get(command.id) === command) {
				commands.delete(command.id);
				bump();
			}
		};
	}

	function pushScope(name: string, opts: { exclusive?: boolean } = {}): () => void {
		const entry = { name, exclusive: opts.exclusive ?? false };
		scopes.push(entry);
		bump();
		return () => {
			const i = scopes.lastIndexOf(entry);
			if (i !== -1) scopes.splice(i, 1);
			bump();
		};
	}

	const activeScopes = (): string[] => {
		// Top first; stop below an exclusive scope.
		const out: string[] = [];
		for (let i = scopes.length - 1; i >= 0; i--) {
			out.push(scopes[i]!.name);
			if (scopes[i]!.exclusive) return out;
		}
		return [...out, GLOBAL];
	};

	const enabled = (c: Command) => (c.when ? c.when() : true);

	function find(combo: string, editable: boolean, overlay: boolean): Command | undefined {
		if (isSingleKey(combo) && (!singleKeys || overlay)) return undefined;
		// eslint-disable-next-line svelte/prefer-svelte-reactivity -- a local lookup, not state
		const conflicting = new Set(
			findConflicts()
				.filter((c) => c.combo === combo)
				.map((c) => c.scope)
		);
		for (const scope of activeScopes()) {
			if (conflicting.has(scope)) return undefined;
			const hit = [...commands.values()].find(
				(c) => (c.scope ?? GLOBAL) === scope && keysOf(c).includes(combo) && enabled(c)
			);
			if (!hit) continue;
			if (editable && !(hit.allowInInput && (combo === 'escape' || hasCommandModifier(combo))))
				return undefined;
			return hit;
		}
		return undefined;
	}

	/** Handles a keydown; true when a command ran (the event is then default-prevented). */
	function handle(event: KeyboardEvent): boolean {
		if (event.defaultPrevented || event.isComposing) return false;
		const hit = find(comboFromEvent(event), isEditable(event.target), inOverlay(event.target));
		if (!hit) return false;
		event.preventDefault();
		hit.run();
		return true;
	}

	/** Listens for keydown on `target`; returns the detach function. */
	function attach(target: Pick<EventTarget, 'addEventListener' | 'removeEventListener'> = window) {
		const listener = (e: Event) => void handle(e as KeyboardEvent);
		target.addEventListener('keydown', listener);
		return () => target.removeEventListener('keydown', listener);
	}

	return {
		register,
		pushScope,
		handle,
		attach,
		/** Every registered command with its effective keys; reactive. */
		list(): CommandInfo[] {
			void version;
			const scopeNames = activeScopes();
			return [...commands.values()].map((c) => ({
				id: c.id,
				title: resolve(c.title),
				section: resolve(c.section),
				scope: c.scope ?? GLOBAL,
				keys: keysOf(c),
				keywords: c.keywords ?? [],
				palette: c.palette !== false,
				active: scopeNames.includes(c.scope ?? GLOBAL) && enabled(c)
			}));
		},
		/** Runs a command by id (the palette); false if unknown or its `when` says no. */
		run(id: string): boolean {
			const c = commands.get(id);
			if (!c || !enabled(c)) return false;
			c.run();
			return true;
		},
		conflicts(): Conflict[] {
			void version;
			return findConflicts();
		},
		/** Rebinds a command; `null` restores its default. Settings persists and replays these. */
		remap(id: string, combos: readonly string[] | null) {
			if (combos === null) overrides.delete(id);
			else
				overrides.set(
					id,
					combos.map((k) => normalizeCombo(k, mac))
				);
			bump();
		},
		get singleKeys() {
			return singleKeys;
		},
		/** WCAG 2.1.4: turn off every bare-character shortcut. */
		setSingleKeys(on: boolean) {
			singleKeys = on;
		}
	};
}

/** The app's registry. Tests create their own with `createKeys()`. */
export const keys = createKeys();
