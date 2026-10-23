// Server message keys → Paraglide messages. Only `settings_*` messages are bundled: the
// generated `message-map.ts` reads each one statically, so the chunk never pulls in the rest
// of the app's strings (and nothing here indexes `m` dynamically).
import { MESSAGES } from './message-map';
import { devWarn } from './schema';
import type { FieldSpec } from './types';

/** The message id of a server key: `.` and `-` become `_`. */
export function messageId(key: string): string {
	return key.replace(/[.-]/g, '_');
}

/** `settings.crawler.intensity.label` → that message's text, or undefined when it's missing. */
export function settingText(key: string): string | undefined {
	const id = messageId(key);
	return Object.hasOwn(MESSAGES, id) ? MESSAGES[id]!() : undefined;
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
