// The watch folder's data-folder check (UP-2), apart from upload.ts so the watch part's lazy chunk
// shares no module with the upload screen's.

/**
 * The data-folder rule the server applies when it watches (`settings/watch.rs` `folder_problem`),
 * read from the two paths as written: a folder that is Lodestar's data folder or inside it, or
 * one that holds it, is never read. A save can't check this (`Section::check` doesn't see the data
 * folder), so the screen says it beside the field. Paths are compared by their parts, so
 * `/data2` isn't inside `/data`; Windows paths (a drive letter or `\\server`) ignore case and
 * take either slash. Links aren't resolved here: the server resolves them and stays the authority,
 * so this can only miss a problem, never invent one for a folder written the same way.
 */
export function dataDirProblem(folder: string, dataDir: string): 'inside' | 'holds' | undefined {
	const f = parts(folder);
	const d = parts(dataDir);
	if (!f || !d || f.windows !== d.windows) return undefined;
	const within = (inner: string[], outer: string[]) =>
		outer.length <= inner.length && outer.every((p, i) => p === inner[i]);
	if (within(f.parts, d.parts)) return 'inside';
	if (within(d.parts, f.parts)) return 'holds';
	return undefined;
}

function parts(path: string): { windows: boolean; parts: string[] } | undefined {
	const raw = path.trim();
	const windows = /^[A-Za-z]:[\\/]/.test(raw) || raw.startsWith('\\\\');
	if (!windows && !raw.startsWith('/')) return undefined;
	const out: string[] = [];
	for (const p of (windows ? raw.toLowerCase() : raw).split(windows ? /[\\/]+/ : /\/+/)) {
		if (p === '' || p === '.') continue;
		// `..` would need the disk to resolve: say nothing rather than guess.
		if (p === '..') return undefined;
		out.push(p);
	}
	return { windows, parts: out };
}
