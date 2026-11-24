// Catalogue ID fields (ENR-11, `imdb:` … `wikidata:` in crates/search/src/query.rs FIELDS): each
// labelled by its catalogue's own name, which isn't translated.
import { m } from '$lib/paraglide/messages.js';

const CATALOGUE = {
	imdb: 'IMDb',
	tmdb: 'TMDB',
	tvdb: 'TheTVDB',
	anidb: 'AniDB',
	anilist: 'AniList',
	mbid: 'MusicBrainz',
	olid: 'Open Library',
	igdb: 'IGDB',
	discogs: 'Discogs',
	steam: 'Steam',
	wikidata: 'Wikidata'
} as const;

export type IdField = keyof typeof CATALOGUE;

/** Each ID field's label ("IMDb ID"), as a message the label tables take. */
export const ID_LABELS = {} as Record<IdField, () => string>;
for (const k of Object.keys(CATALOGUE) as IdField[])
	ID_LABELS[k] = () => m.search_filters_id({ name: CATALOGUE[k] });
