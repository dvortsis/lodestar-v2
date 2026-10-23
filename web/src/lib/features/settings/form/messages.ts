// Server message keys → Paraglide messages. Only `settings_*` messages are bundled, and a screen
// loads just its own sections' (`loadSettingMessages`, which resolves before its form mounts):
// the generated `message-map/` reads each one statically, so a chunk never pulls in the rest of
// the app's strings (and nothing here indexes `m` dynamically).
import { COMMON_UNIT, LOADERS } from './message-map';
import { devWarn } from './schema';
import type { FieldSpec } from './types';

/** The message id of a server key: `.` and `-` become `_`. */
export function messageId(key: string): string {
	return key.replace(/[.-]/g, '_');
}

/** The messages loaded so far, by message id. Tests seed it. */
export const LOADED = new Map<string, () => string>();
const loading = new Map<string, Promise<void>>();

/**
 * Loads the messages of these sections (and the group headings every section shares). Resolves
 * once they are readable; an unknown section, or one that fails to load, reads as missing.
 */
export async function loadSettingMessages(sections: Iterable<string>): Promise<void> {
	await Promise.all(
		[COMMON_UNIT, ...sections].map((unit) => {
			if (!Object.hasOwn(LOADERS, unit)) return undefined;
			let p = loading.get(unit);
			if (!p) {
				p = LOADERS[unit]!().then(
					(mod) => {
						for (const [id, f] of Object.entries(mod.MESSAGES)) LOADED.set(id, f);
					},
					() => {
						loading.delete(unit);
					}
				);
				loading.set(unit, p);
			}
			return p;
		})
	);
}

/** `settings.crawler.intensity.label` → that message's text, or undefined when it's missing. */
export function settingText(key: string): string | undefined {
	return LOADED.get(messageId(key))?.();
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
