import { m } from '$lib/paraglide/messages.js';

const TYPE: Record<string, () => string> = {
	movie: () => m.content_type_movie(),
	tv: () => m.content_type_tv(),
	music: () => m.content_type_music(),
	audiobook: () => m.content_type_audiobook(),
	ebook: () => m.content_type_ebook(),
	comic: () => m.content_type_comic(),
	software: () => m.content_type_software(),
	game: () => m.content_type_game(),
	images: () => m.content_type_images(),
	video: () => m.content_type_video(),
	other: () => m.content_type_other(),
	adult: () => m.content_type_adult()
};
const ADULT: Record<string, () => string> = {
	scene: () => m.adult_kind_scene(),
	movie: () => m.adult_kind_movie(),
	photos: () => m.adult_kind_photos(),
	comics: () => m.adult_kind_comics(),
	game: () => m.adult_kind_game(),
	vr: () => m.adult_kind_vr(),
	audio: () => m.adult_kind_audio()
};

/** The label for a content type, or for an adult kind with `'adult'`. Unknown values show as sent. */
export function contentTypeLabel(value: string, kind: 'type' | 'adult' = 'type'): string {
	return (kind === 'adult' ? ADULT : TYPE)[value]?.() ?? value;
}

const VIA: Record<string, () => string> = {
	files: () => m.detail_via_files(),
	folder: () => m.detail_via_folder(),
	structure: () => m.detail_via_structure()
};

export const resolvedVia = (value: string): string => VIA[value]?.() ?? value;
