// The changes this screen starts, as typed literals (`Change` is a type-only import). They have the
// shapes of `flows/changes.ts`' builders, which this screen doesn't import: that module is shared
// with the dialogs that other screens load ("Block this…", the 18+ confirmation), and a static import
// would put it, and every dialog that uses it, in this screen's chunk. The server judges each
// change; nothing here does.
import type { Change } from '../flows/types';

export const coreSwitch = (rule: string, on: boolean): Change => ({ op: 'core_switch', rule, on });
export const packSwitch = (id: string, on: boolean): Change => ({ op: 'pack_switch', id, on });
export const addRule = (query: string, action: string): Change => ({
	op: 'add_rule',
	query,
	action
});
export const setRuleEnabled = (id: number, enabled: boolean): Change => ({
	op: 'update_rule',
	id,
	enabled
});
export const removeRule = (id: number): Change => ({ op: 'remove_rule', id });
export const removeOverride = (id: number): Change => ({ op: 'remove_override', id });
export const removeException = (id: number): Change => ({ op: 'remove_exception', id });
export const addAllow = (query: string): Change => ({ op: 'add_allow', query });
export const removeAllow = (id: number): Change => ({ op: 'remove_allow', id });
export const release = (hash: string): Change => ({ op: 'release', hash });
export const confirmAbuse = (hash: string): Change => ({ op: 'confirm', hash });
export const falsePositive = (hash: string): Change => ({ op: 'false_positive', hash });
export const floorRemoval = (source: string, remove: boolean): Change => ({
	op: 'resolve_floor_removal',
	source,
	remove
});
export const undoChange = (): Change => ({ op: 'undo' });
