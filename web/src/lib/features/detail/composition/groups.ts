import type { Group } from '$lib/charts/shared';
import { GROUP_ORDER } from '$lib/charts/shared';
import { m } from '$lib/paraglide/messages.js';

const LABEL: Record<Group, () => string> = {
	video: () => m.video(),
	audio: () => m.audio(),
	images: () => m.images(),
	documents: () => m.documents(),
	archives: () => m.archives(),
	software: () => m.software(),
	other: () => m.other()
};

/** A group name the server sent, narrowed; anything unknown is "other" (the neutral slot). */
export const groupOf = (value: string): Group =>
	(GROUP_ORDER as readonly string[]).includes(value) ? (value as Group) : 'other';

export const groupLabel = (group: Group): string => LABEL[group]();
