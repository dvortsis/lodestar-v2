// External links for the ids in "Identified as" (§5.5). A link is built only from an id that
// matches the strict shape of its scheme, into a fixed https template per scheme. A URL string from
// the API is never used, and an unknown scheme gets no link.
const num = /^\d{1,10}$/;
const SOURCES: Record<string, { pattern: RegExp; url: (id: string) => string; name: string }> = {
	imdb: {
		name: 'IMDb',
		pattern: /^tt\d{7,10}$/,
		url: (id) => `https://www.imdb.com/title/${encodeURIComponent(id)}/`
	},
	tvdb: {
		name: 'TVDB',
		pattern: num,
		url: (id) => `https://thetvdb.com/dereferrer/series/${encodeURIComponent(id)}`
	},
	anidb: {
		name: 'AniDB',
		pattern: num,
		url: (id) => `https://anidb.net/anime/${encodeURIComponent(id)}`
	},
	anilist: {
		name: 'AniList',
		pattern: num,
		url: (id) => `https://anilist.co/anime/${encodeURIComponent(id)}`
	},
	wikidata: {
		name: 'Wikidata',
		pattern: /^Q\d{1,12}$/,
		url: (id) => `https://www.wikidata.org/wiki/${encodeURIComponent(id)}`
	}
	// tmdb and mbid are left out: their links need a movie/tv or entity kind the API doesn't send.
};

/** The link for one id, or null (unknown scheme, or a value that doesn't have the scheme's shape). */
export function idLink(scheme: string, value: string): { href: string; name: string } | null {
	const s = Object.hasOwn(SOURCES, scheme) ? SOURCES[scheme] : undefined;
	if (!s || !s.pattern.test(value)) return null;
	return { href: s.url(value), name: s.name };
}
