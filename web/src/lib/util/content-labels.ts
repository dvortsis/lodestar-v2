// Shared by result rows, Discover tabs and parsed detail. Keep message functions
// (not translated strings), so switching locale still updates every consumer.
import { m } from '$lib/paraglide/messages.js';

export const TYPE_LABELS: Record<string, () => string> = {
	movie: m.content_type_movie,
	tv: m.content_type_tv,
	music: m.content_type_music,
	audiobook: m.content_type_audiobook,
	ebook: m.content_type_ebook,
	comic: m.content_type_comic,
	software: m.content_type_software,
	game: m.content_type_game,
	images: m.content_type_images,
	video: m.content_type_video,
	other: m.content_type_other,
	adult: m.content_type_adult
};

export const ADULT_LABELS: Record<string, () => string> = {
	scene: m.adult_kind_scene,
	movie: m.adult_kind_movie,
	photos: m.adult_kind_photos,
	comics: m.adult_kind_comics,
	game: m.adult_kind_game,
	vr: m.adult_kind_vr,
	audio: m.adult_kind_audio
};
