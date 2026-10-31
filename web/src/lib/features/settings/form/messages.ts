// Server message keys → Paraglide messages. Only `settings_*` messages are bundled: the
// generated `message-maps/<section>.ts` read each one statically, so a chunk never pulls in the
// rest of the app's strings (and nothing here indexes `m` dynamically). A section's map loads
// with its form (`loadMessages`, awaited by `SettingsForm` before its fields render), so the
// engine carries none of the server labels.
import { LOADERS } from './message-map';
import { devWarn } from './schema';
import type { FieldSpec } from './types';

/** The message id of a server key: `.` and `-` become `_`. */
export function messageId(key: string): string {
	return key.replace(/[.-]/g, '_');
}

type MessageMap = Record<string, () => string>;
const maps: Record<string, MessageMap> = {};
const loading: Record<string, Promise<void>> = {};

/** The map a server key lives in: its section, or `group` for a group heading. */
const mapOf = (key: string) => key.split('.')[1] ?? '';

/** The maps a form of these sections needs: theirs, and the group headings'. */
const needed = (sections: Iterable<string>) => [...new Set([...sections, 'group'])];

/** Whether these sections' maps are all in, so a render can't show a raw key. */
export function messagesLoaded(sections: Iterable<string>): boolean {
	return needed(sections).every((s) => s in maps || !Object.hasOwn(LOADERS, s));
}

/**
 * Loads these sections' maps (and the group headings'). A map that can't load leaves its keys
 * missing, which show as the field's name (browsers keep a failed chunk, so it isn't retried).
 */
export async function loadMessages(sections: Iterable<string>): Promise<void> {
	await Promise.all(
		needed(sections)
			.filter((s) => Object.hasOwn(LOADERS, s))
			.map(
				(s) =>
					(loading[s] ??= LOADERS[s]!().then(
						(mod) => {
							maps[s] = mod.MESSAGES;
						},
						() => {
							maps[s] = {};
						}
					))
			)
	);
}

/** `settings.crawler.intensity.label` → that message's text, or undefined when it's missing. */
export function settingText(key: string): string | undefined {
	const id = messageId(key);
	const map = maps[mapOf(key)];
	return map && Object.hasOwn(map, id) ? map[id]!() : undefined;
}

const warned = new Set<string>();
function warnOnce(id: string, message: string) {
	if (warned.has(id)) return;
	warned.add(id);
	devWarn(message);
}

/** The field's label; its `name` when the message is missing. */
export function fieldLabel(spec: FieldSpec): string {
	const text = settingText(spec.labelKey);
	if (import.meta.env.DEV && text === undefined)
		warnOnce(spec.labelKey, `no message for ${spec.labelKey}`);
	return text ?? spec.name;
}

/** The field's help; undefined (no help shown) when the message is missing. */
export function fieldHelp(spec: FieldSpec): string | undefined {
	const text = settingText(spec.helpKey);
	if (import.meta.env.DEV && text === undefined)
		warnOnce(spec.helpKey, `no message for ${spec.helpKey}`);
	return text;
}
