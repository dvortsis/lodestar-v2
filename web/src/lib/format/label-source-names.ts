// Discovery sources by plugin id alone (X-labels), for Insights, whose segments carry no names:
// the plugins' manifest names, which `labels.test.ts` matches against `crates/sources-plugins`.
import { own } from './label-shared';
import { sourceLabel } from './label-source';

const NAMES: Record<string, string> = {
	academic_torrents: 'Academic Torrents',
	anidb: 'AniDB',
	anilist: 'AniList',
	archive_org: 'Internet Archive',
	arr_webhooks: 'Sonarr and Radarr webhooks',
	discogs: 'Discogs',
	download_clients: 'Your download clients',
	full_scrape: 'Tracker full scrape',
	igdb: 'IGDB',
	musicbrainz: 'MusicBrainz',
	nyaa: 'Nyaa',
	openlibrary: 'Open Library',
	predb: 'PreDB',
	prowlarr: 'Prowlarr / Jackett',
	srrdb: 'srrDB',
	stashdb: 'StashDB',
	steam: 'Steam',
	sukebei: 'Sukebei',
	tmdb: 'TMDB',
	torrent_cache: 'Torrent cache',
	tpdb: 'ThePornDB',
	tvdb: 'TheTVDB',
	wikidata: 'Wikidata'
};

/** A source by plugin id, with no name from the server: its manifest name, else `sourceLabel`'s. */
export const sourceIdLabel = (id: string): string => sourceLabel(id, own(NAMES, id));
