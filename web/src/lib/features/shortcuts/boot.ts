// Starts the keyboard preferences: registers the shortcut sheet and the palette toggle, applies the
// saved mapping (a per-viewer cache first, so the key that woke the shell already sees it, then the
// server's), and follows changes made elsewhere. The shell calls `startShortcuts()` on the first
// interaction. Nothing here imports the sheet, the screen or the editor.
import { mount, unmount } from 'svelte';
import { api, latest } from '$lib/api/client';
import { events } from '$lib/state/events.svelte';
import { keys } from '$lib/keys';
import { m } from '$lib/paraglide/messages.js';
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { ACTION_IDS } from './actions';
import { onceLive } from './follow.svelte';
import { sheet } from './sheet.svelte';

export interface Prefs {
	shortcuts: boolean;
	shortcut_keys: Record<string, string>;
}

const CACHE = 'lodestar.shortcuts';
const FALLBACK_MS = 10_000;
const ACTION = /^[a-z][a-z0-9_.]*$/;
export const SECTION = { path: { section: 'preferences' } };

let warned = false;
const warn = (what: string) => {
	if (!warned) console.warn(`shortcuts: ignoring a malformed ${what}`);
	warned = true;
};

/** Reads the preferences out of untrusted JSON: a boolean and a record of id → string. */
export function narrow(shortcuts: unknown, shortcutKeys: unknown): Prefs | undefined {
	const ok =
		typeof shortcuts === 'boolean' &&
		typeof shortcutKeys === 'object' &&
		shortcutKeys !== null &&
		!Array.isArray(shortcutKeys) &&
		Object.entries(shortcutKeys).every(
			([id, key]) => ACTION.test(id) && typeof key === 'string' && key !== ''
		);
	if (!ok) return void warn('shortcut setting');
	return { shortcuts, shortcut_keys: { ...(shortcutKeys as Record<string, string>) } };
}

export const fromFields = (fields: Record<string, { value?: unknown } | undefined>) =>
	narrow(fields.shortcuts?.value, fields.shortcut_keys?.value);

function readCache(): Prefs | undefined {
	try {
		const raw = JSON.parse(localStorage.getItem(CACHE) ?? 'null') as Partial<Prefs> | null;
		return raw ? narrow(raw.shortcuts, raw.shortcut_keys) : undefined;
	} catch {
		return undefined;
	}
}

function writeCache(prefs: Prefs) {
	try {
		localStorage.setItem(CACHE, JSON.stringify(prefs));
	} catch {
		/* storage blocked: the server stays the source of truth */
	}
}

/** Puts a mapping into the registry (and the cache). Actions it leaves out get their default. */
export function applyPrefs(prefs: Prefs, cache = true) {
	keys.setSingleKeys(prefs.shortcuts);
	for (const id of ACTION_IDS) if (!(id in prefs.shortcut_keys)) keys.remap(id, null);
	for (const [id, key] of Object.entries(prefs.shortcut_keys)) {
		// Only the remappable actions: a stored `palette.open` must not take Ctrl+K away.
		if (!(ACTION_IDS as readonly string[]).includes(id)) continue;
		try {
			keys.remap(id, [key]);
		} catch {
			warn('shortcut key');
		}
	}
	if (cache) writeCache(prefs);
}

const slot = latest();

/** Fetches the saved preferences and applies them. A failure keeps what is applied. */
export async function refresh(): Promise<void> {
	try {
		const res = await slot.run((signal) => api.get('/settings/{section}', { ...SECTION, signal }));
		const prefs = fromFields(res.fields);
		if (prefs) applyPrefs(prefs);
	} catch {
		// A 401 is the shell's; offline or superseded keeps what is applied, and the keys still work.
	}
}

let instance: ReturnType<typeof mount> | undefined;
let loading = false;

/** Opens the sheet, loading it the first time. */
export function openSheet() {
	sheet.open = true;
	if (instance || loading) return;
	loading = true;
	void import('./ShortcutSheet.svelte').then(
		(mod) => {
			loading = false;
			instance = mount(mod.default, { target: document.body });
		},
		() => {
			// The chunk didn't load (offline, a redeploy): let the next `?` try again.
			loading = false;
			sheet.open = false;
		}
	);
}

export function startShortcuts(): () => void {
	const cached = readCache();
	if (cached) applyPrefs(cached, false);
	let fetched = false;
	const fetchOnce = () => {
		if (!fetched) void refresh();
		fetched = true;
	};
	// Not before the shell's stream is live: a request that succeeds tells the shell the server is
	// up. A blocked stream falls back to a timer; the sign-in screens open no stream (`stopped`)
	// and have no session to ask.
	const timer = setTimeout(() => events.status !== 'stopped' && fetchOnce(), FALLBACK_MS);
	const offs = [
		keys.register({
			id: 'help.shortcuts',
			title: () => m.shortcuts_action_help_shortcuts(),
			section: () => m.shortcuts_section_general(),
			run: openSheet
		}),
		keys.register({
			id: 'shortcuts.single_keys',
			title: () => (keys.singleKeys ? m.shortcuts_cmd_turn_off() : m.shortcuts_cmd_turn_on()),
			section: () => m.shortcuts_section_general(),
			keys: [],
			keywords: ['keyboard', 'shortcuts', 'wcag'],
			run: () =>
				void import('./toggle').then(
					(t) => t.toggleSingleKeys(),
					() => toast.push({ kind: 'error', text: m.shortcuts_toast_failed() })
				)
		}),
		onceLive(() => {
			fetchOnce();
			return events.on(
				'settings',
				(e) => {
					if (e.section === 'preferences') void refresh();
				},
				// A change made while the stream was down is only found by asking.
				{ onResync: () => void refresh() }
			);
		})
	];
	return () => {
		clearTimeout(timer);
		offs.forEach((off) => off());
		if (instance) void unmount(instance);
		instance = undefined;
		sheet.open = false;
	};
}
