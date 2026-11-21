// Content types (glossary `content_type`, `adult_kind`) and file-type groups (`category_group`).
import { m } from '$lib/paraglide/messages.js';
import { ADULT_LABELS, TYPE_LABELS } from '$lib/util/content-labels';
import { word } from './label-shared';

/** A content type (glossary `content_type`): `ebook` → "Ebook". */
export const typeLabel = (value: string): string => word(TYPE_LABELS, value);

/** An adult type (glossary `adult_kind`), a formatter of its own so other screens don't carry it. */
export const adultTypeLabel = (value: string): string => word(ADULT_LABELS, value);

// The file-type groups a composition is coloured by (glossary `category_group`).
const FILE_GROUP: Record<string, () => string> = {
	video: m.video,
	audio: m.audio,
	images: m.images,
	documents: m.documents,
	archives: m.archives,
	software: m.software,
	other: m.other
};
/** A file-type group (`video` → "Video"); "82% video" in a sentence lower-cases it (the glossary). */
export const fileGroupLabel = (group: string): string => word(FILE_GROUP, group);
