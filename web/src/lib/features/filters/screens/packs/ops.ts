// The changes the packs panel starts, as typed literals (see ../ops.ts for why they aren't the
// flows' builders). The server judges each: whether an acceptance or override weakens, and so asks
// for the password, is its answer to the preview and never decided here.
import type { Change } from '../../flows/types';

export const switchRule = (id: string, on: boolean): Change => ({ op: 'pack_switch', id, on });
export const acceptVersions = (rules: string[]): Change => ({ op: 'accept_pack_versions', rules });
export const setOverride = (rule: string, action: string, unless: string): Change => ({
	op: 'set_pack_override',
	rule,
	action,
	unless
});
export const removeOverride = (rule: string): Change => ({ op: 'remove_pack_override', rule });
export const confirmOverride = (rule: string): Change => ({ op: 'confirm_pack_override', rule });
