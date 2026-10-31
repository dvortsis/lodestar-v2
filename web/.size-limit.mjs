// §10.1 budgets, gzipped. Size only: the preset-app timing step is disabled (`running: false`).
// Route budgets come from the SvelteKit build manifest (scripts/size-entries.mjs), so later
// route chunks never count against the search route.
import { existsSync, readFileSync } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { checkFirstLoad, preloadedJs } from './scripts/first-load.mjs';
import { requireMeasuredJs, routeFiles } from './scripts/size-entries.mjs';

const search = routeFiles('/search');

// Fonts are committed subsets (scripts/fonts.mjs) served from static/fonts -> build/fonts.
const FONT_DIR = 'build/fonts';
const font = (name) => `${FONT_DIR}/${name}.woff2`;
const firstPaintFonts = [font('atkinson-hyperlegible-next-latin')];
const monoFonts = [font('atkinson-hyperlegible-mono-latin')];

// Favicons are exempt: the head links to them (apple-touch-icon.png) are fetched by the browser
// chrome, not rendered on the page.
// §10.17 brand gate: no brand raster (/brand/**/*.{avif,webp,png}) may be in the first-load set.
// Scenes load lazily with the states that use them; a raster referenced from the html, the search
// route's JS or its CSS would be fetched (or preloaded) on first paint. Throwing here fails
// `npx size-limit` before any size is measured.
const BRAND_RASTER = /\/brand\/(?!favicons\/)[^"'`\s)]*\.(?:avif|webp|png)/g;
const firstLoad = ['build/index.html', ...search.js, ...search.css].filter((f) => existsSync(f));
const leaks = firstLoad.flatMap((file) =>
	[...new Set(readFileSync(file, 'utf8').match(BRAND_RASTER) ?? [])].map((ref) => `${file}: ${ref}`)
);
if (leaks.length) {
	throw new Error(`Brand rasters in the first-load set (§10.17):\n  ${leaks.join('\n  ')}`);
}

// §10.1: the search route's first load holds only modules it reaches itself (no other route's code,
// no Svelte runtime only another route uses). scripts/first-load.mjs explains the rules.
const strays = checkFirstLoad('/search', search.js);
if (strays.length)
	throw new Error(
		`The search route's first load carries code it doesn't use:\n  ${strays.join('\n  ')}`
	);

// The route table (kit's app.js), budgeted below.
const appEntry = search.js.find((f) => /\/entry\/app\.[^/]*\.js$/.test(f));
// No file on the first load lists JS to preload (scripts/first-load.mjs `routePreloads`): the route
// table's list grew with every route, and a route node's with every chunk its lazy parts use
// (S-search-2's results panel).
const preloaded = search.js.flatMap((f) =>
	preloadedJs(readFileSync(f, 'utf8')).map((js) => `${f}: ${js}`)
);
if (preloaded.length)
	throw new Error(
		`The search route's first load preloads JS it doesn't run yet:\n  ${preloaded.join('\n  ')}`
	);

const stageLazy = [/uplot/i];
const entry = (name, path, limit) => ({ name, path, limit, gzip: true, running: false });

// A route's own JS and CSS is its first view (runner decision, §2.19): static imports, plus dynamic
// imports that run at mount (a route that defers its screen with `import()` would otherwise
// measure as its stub). Chunks loaded only on interaction (opening a dialog or a menu) are left
// out with `onDemand`, a list of regexes on manifest keys: the walk stops at a matching chunk and
// `onDemandOf` measures what it stopped at, so the lazy code is still gated by its own entry.
// Chunks the search route already loads are never walked into, so the entry's dynamic imports
// (every other route's node) are not followed.
const manifest = existsSync('.svelte-kit/output/client/.vite/manifest.json')
	? JSON.parse(readFileSync('.svelte-kit/output/client/.vite/manifest.json', 'utf8'))
	: {};
const keyOf = new Map(Object.entries(manifest).map(([key, chunk]) => [`build/${chunk.file}`, key]));
const walk = (route, onDemand, collectOnDemand, beyond = []) => {
	const mine = routeFiles(route);
	const shared = new Set([...search.js, ...search.css, ...beyond]);
	const seen = new Set();
	const files = new Set();
	const later = new Set();
	const matches = (key) => onDemand.some((re) => re.test(key));
	const visit = (key, demanded) => {
		const chunk = manifest[key];
		if (!chunk || shared.has(`build/${chunk.file}`)) return;
		const demand = demanded || matches(key);
		const id = `${demand}:${key}`;
		if (seen.has(id)) return;
		seen.add(id);
		(demand ? later : files).add(`build/${chunk.file}`);
		for (const css of chunk.css ?? [])
			if (!shared.has(`build/${css}`)) (demand ? later : files).add(`build/${css}`);
		for (const next of [...(chunk.imports ?? []), ...(chunk.dynamicImports ?? [])])
			visit(next, demand);
	};
	for (const f of mine.js) if (!shared.has(f) && keyOf.has(f)) visit(keyOf.get(f), false);
	for (const f of mine.css) if (!shared.has(f)) files.add(f);
	return collectOnDemand ? [...later].filter((f) => !files.has(f)) : [...files];
};
const own = (route, { onDemand = [], beyond = [] } = {}) => walk(route, onDemand, false, beyond);
// A lazily imported module (a manifest key), measured the same way: what the search route doesn't
// already load, through static imports and mount-time dynamic imports. A shared chunk has no source
// key, so `onDemand` also tests `chunk:<its name>` (e.g. /^chunk:magnet$/).
// `beyond` lists files the view has already loaded when this chunk loads (e.g. `viewOf(...)`): they
// are left out like the first load's, so the entry measures only what this chunk adds.
const ownChunk = (sourceKey, onDemand = [], collectOnDemand = false, beyond = []) => {
	const shared = new Set([...search.js, ...search.css, ...beyond]);
	const seen = new Set();
	const now = new Set();
	const later = new Set();
	const matches = (key, chunk) =>
		onDemand.some((re) => re.test(key) || (!!chunk.name && re.test(`chunk:${chunk.name}`)));
	const visit = (key, demanded) => {
		const chunk = manifest[key];
		if (!chunk || shared.has(`build/${chunk.file}`)) return;
		const demand = demanded || matches(key, chunk);
		const id = `${demand}:${key}`;
		if (seen.has(id)) return;
		seen.add(id);
		const into = demand ? later : now;
		into.add(`build/${chunk.file}`);
		for (const css of chunk.css ?? []) if (!shared.has(`build/${css}`)) into.add(`build/${css}`);
		for (const next of [...(chunk.imports ?? []), ...(chunk.dynamicImports ?? [])])
			visit(next, demand);
	};
	visit(sourceKey, false);
	return collectOnDemand ? [...later].filter((f) => !now.has(f)) : [...now];
};
const onDemandOf = (route, onDemand) => walk(route, onDemand, true);

// Detail: the DetailHost module, and the sections that load when scrolled to (or on an error).
const DRAWER = 'src/lib/features/detail/DetailHost.svelte';
// The key commands load after mount (with the key registry and the magnet code), so they are on demand too.
const detailLazy = [
	/features\/detail\/(Contents|ErrorState|parsed\/Parsed)\.svelte$/,
	// Clipboard failure is an interaction, not part of opening the detail view.
	// Its UI and mount runtime remain included in the on-demand budget below.
	/features\/detail\/manual-mount\.ts$/,
	/features\/detail\/magnet\.ts$/,
	/^chunk:magnet$/,
	/features\/detail\/commands\.ts$/
];

// Settings: the form engine loads these on demand (its README), so a screen's first view stops at
// them: every control but the toggle and the text input (per type), instant mode, previews, the
// field menu, the ⌘S registration, copy's toast and the confirmation dialog. The section entry uses
// this list; "Settings controls (on demand)" gates what it leaves out.
const settingsOnDemand = [
	/features\/settings\/form\/(Number|Slider|Choice|Secret|Chips|Json)Control\.svelte$/,
	/features\/settings\/form\/(instant|preview|menu|save-key|copy)\.ts$/,
	/features\/settings\/form\/ConfirmDialog\.svelte$/
];
// The edit chunk (owner, 2026-10-24; docs/decisions/2026-10-30-S-settings-reconcile.md): the save
// pipeline and what it shows load with the first edit, a stored draft or an event. It is gated on
// its own, so "Settings controls (on demand)" keeps measuring only the controls.
// The digest preview (S-insights-3) loads after the Insights & digest screen mounts and fetches on
// a button press, so every first view stops at it; "Insights digest preview (on demand)" gates it.
const insightsDigest = [/features\/insights\/digest\/DigestPreview\.svelte$/];
const settingsEdit = [/features\/settings\/form\/edit\.ts$/];
// The connection screens' parts (S-settings-2b): the controls that stand in for the JSON fallback,
// the per-group test, the Clear usage data panel and the Privacy dialog load once their fields are
// on the page or on an interaction, so a first view stops at them. Gated on their own below.
const settingsConnections = [
	/features\/settings\/screens2\/(GroupTest|ClearUsage|ClientsWidget|ClientSelectWidget|ChannelsWidget|Advanced|LoadFailed)\.svelte$/,
	/features\/settings\/screens2\/privacy\.ts$/,
	// The four navigation icons, loaded after the navigation shows (`Screen.iconLoad`).
	/features\/settings\/screens2\/icons\.ts$/
];
// The server labels, one generated map per section (M-settings-engine-2): a form loads its
// section's map before its fields render, so a first view stops at them. Gated on their own.
const settingsMessages = [/features\/settings\/form\/message-maps\/\w+\.ts$/];
// A section's first view does include the maps its form awaits before any field renders: the
// group headings' and the section's own. A generated screen could be any generated section, so its
// gate counts the group map and the largest of those sections' maps (the worst case, by gzip). A
// hand-built screen names the sections its body shows (`bodyMaps`), so it counts its own and the
// group map. Every other map is left out; a later screen's map is gated the moment it lands.
// "Settings messages (on demand)" and the connection maps' entry measure them all.
const messageKeys = Object.keys(manifest).filter((key) =>
	settingsMessages.some((re) => re.test(key))
);
const mapName = (key) =>
	key
		.split('/')
		.at(-1)
		.replace(/\.(ts|svelte)$/, '');
const gzSize = (key) =>
	gzipSync(readFileSync(`.svelte-kit/output/client/${manifest[key].file}`)).length;
const bodyMaps = {
	AppearanceScreen: ['preferences'],
	KeyboardScreen: ['preferences'],
	LanguageScreen: ['preferences'],
	IntegrationsScreen: ['integrations'],
	NotificationsScreen: ['notifications'],
	InsightsScreen: ['insights'],
	// Security reads no section map: secure cookies is a status line, not an engine field.
	SecurityScreen: [],
	LoggingScreen: ['logging']
};
// A generated screen can show any section no hand-built body names, so a new section's map joins the
// worst case the moment it lands.
// The outputs' map (S-logs-2b) belongs to Logging's body too: it loads with its outputs part.
const handBuilt = new Set([...Object.values(bodyMaps).flat(), 'logoutput']);
const largestMap = mapName(
	messageKeys
		.filter((key) => mapName(key) !== 'group' && !handBuilt.has(mapName(key)))
		.sort((a, b) => gzSize(b) - gzSize(a))[0]
);
const mapsLeftOut = (keep) =>
	messageKeys
		.filter((key) => mapName(key) !== 'group' && !keep.includes(mapName(key)))
		.map((key) => new RegExp(`${key.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&')}$`));
const settingsMessagesLeftOut = mapsLeftOut([largestMap]);
const settingsLazy = requireMeasuredJs(
	'Settings controls (on demand)',
	onDemandOf('/settings/[section]', settingsOnDemand).filter(
		(f) =>
			!onDemandOf('/settings/[section]', settingsEdit).includes(f) &&
			!onDemandOf('/settings/[section]', settingsMessages).includes(f)
	)
);
// The four connection sections' maps (S-settings-2b) have an entry of their own, so the existing
// maps' entry keeps its limit; the group map stays in the existing one.
const connectionMaps = [
	/features\/settings\/form\/message-maps\/(trackers|integrations|notifications|insights)\.ts$/
];
const connectionMapFiles = onDemandOf('/settings/[section]', connectionMaps);
// Logging's map (S-logs-2) has its own entry too, so the existing maps' entries keep their limits.
// The outputs' labels (S-logs-2b): their own map, loaded with the outputs after Logging's first view.
const loggingOutputMapFiles = onDemandOf('/settings/[section]', [
	/features\/settings\/form\/message-maps\/logoutput\.ts$/
]);
const loggingMapFiles = onDemandOf('/settings/[section]', [
	/features\/settings\/form\/message-maps\/logging\.ts$/
]);
const settingsMessagesLazy = requireMeasuredJs(
	'Settings messages (on demand)',
	onDemandOf('/settings/[section]', settingsMessages).filter(
		(f) =>
			!connectionMapFiles.includes(f) &&
			!loggingMapFiles.includes(f) &&
			!loggingOutputMapFiles.includes(f)
	)
);
const settingsConnectionMaps = requireMeasuredJs(
	'Settings connection messages (on demand)',
	connectionMapFiles
);
const settingsConnectionsLazy = requireMeasuredJs(
	'Settings connection screens (on demand)',
	onDemandOf('/settings/[section]', settingsConnections)
);
const insightsDigestLazy = requireMeasuredJs(
	'Insights digest preview (on demand)',
	onDemandOf('/settings/[section]', insightsDigest)
);
// The /insights print layout (S-insights-3): its markup, theme swap and print.css, loaded after the
// overview mounts; measured beyond the overview's own chunks.
const OVERVIEW = 'src/lib/features/insights/overview/Overview.svelte';
const PRINT = 'src/lib/features/insights/print/PrintReport.svelte';
const insightsPrintLazy = requireMeasuredJs(
	'Insights print layout (on demand)',
	ownChunk(PRINT, [], false, ownChunk(OVERVIEW, [new RegExp(`${PRINT.replaceAll('/', '\\/')}$`)]))
);
const settingsEditLazy = requireMeasuredJs(
	'Settings edit chunk (on demand)',
	onDemandOf('/settings/[section]', settingsEdit)
);
// Settings screens (S-settings-reconcile): the registry (shell/screens.ts) imports every screen.ts
// eagerly. A generated screen's form (`SectionForm`, with the engine) and a hand-built screen's body
// (`load`, named `*Screen.svelte`, e.g. Keyboard's) are dynamic imports made at mount on their own
// kind of screen; a screen's extras (`extra`, General's export) load after the form.
// - "Settings section" is a generated screen's first view: the route, the form and the engine.
// - Each hand-built screen's first view is the route without the form, plus its body: gated against
//   the 45 KB route cap (§10.1), one entry per body, so a later screen is gated the moment it lands.
// - "Settings screen extras (on demand)" gates the extras beyond the section's first view.
const SECTION_FORM = 'src/lib/features/settings/shell/SectionForm.svelte';
// The shell's section form, which a body such as Language & region shows for its fields: gated
// as "Settings section", so a body that loads it on demand stops at it.
const settingsFormChunk = [new RegExp(`${SECTION_FORM.replaceAll('/', '\\/')}$`)];
// Language & region's form: its body loads it at once, so its first view includes it, and its
// extras entry stops at it.
const languageForm = [/features\/settings\/screens2\/LanguageForm\.svelte$/];
const settingsBodies = [/features\/[^/]+\/(?:[^/]+\/)?[A-Z]\w*Screen\.svelte$/];
const settingsExtras = [/features\/settings\/shell\/SettingsExport\.svelte$/];
// What a hand-built body loads only on an interaction: Keyboard's shortcut sheet (opened with ?) and
// its single-key toggle command (S-keys' boot.ts). Gated with the extras.
const settingsBodyInteractions = [/features\/shortcuts\/(ShortcutSheet\.svelte|toggle\.ts)$/];
// Security (M-security-settings): the password confirmation and its lock-out banner load on the
// first risky action, and the navigation icon after the navigation shows, so a first view stops at
// them; gated on their own below. `body.ts` only forwards to the body when Security opens: other
// screens never load it, and Security's first view is measured from the body itself.
const settingsSecurity = [
	/features\/settings\/security\/(PasswordDialog|Lockout)\.svelte$/,
	/features\/settings\/security\/icon\.ts$/
];
const securityLoader = [/features\/settings\/(security|logging)\/body\.ts$/];
// Security part 2 (M-security-settings-2): the key manager, the proxy section and the secret's
// reveal dialog load after the first view; gated on their own below.
const settingsSecurityLater = [/features\/settings\/security\/(Keys|Proxy|Reveal)\.svelte$/];
// About (S-about): the diagnostics block with Report a problem and the licence list load when their
// buttons are pressed, and its navigation icon and purpose line (for the filter) on demand; gated on
// their own below. The parts only load from About's body.
const settingsAboutModules = [/features\/settings\/about\/(icon|about)\.ts$/];
const settingsAboutParts = [
	/features\/settings\/about\/(Diagnostics|Licences)\.svelte$/,
	...settingsAboutModules
];
// Logging (S-logs-2): the Verbosity control, the event list and the component levels (loaded when
// their fields are on the page), `parts.ts` (the catalog, the banners and the data kinds, which all
// wait on `GET /logs/catalog`), the privacy requests with their dialogs, and the navigation icon load
// after the first view; gated on their own below. `/logs` loads the privacy requests on Turn off now.
const settingsLogging = [
	/features\/settings\/logging\/(EventList|ComponentLevels|VerbosityWidget)\.svelte$/,
	/features\/settings\/logging\/(privacy|icon|parts)\.ts$/
];
// Logging part 2 (S-logs-2b): the log files with the Danger zone, the preview, and the console
// detail's question load after the first view (or on the first save that needs it).
const settingsLoggingFiles = [
	/features\/settings\/logging\/(LogFiles|Preview|Outputs|Tail)\.svelte$/,
	/features\/settings\/logging\/outputs\.ts$/
];
const logsLater = [/features\/settings\/logging\/(privacy|catalog\.svelte)\.ts$/];
// The registry's purpose lines and notices (`Screen.about`, X-settings-registry-slim): one small
// module per registry file, loaded when a generated screen opens (so "Settings section" counts them)
// and when the filter is first used. The overview and a hand-built screen's first view stop at them;
// "Settings screen purposes (on demand)" gates them. Named per registry file on purpose, so no
// other `about.ts` is left out of a first view: a new registry file's `about.ts` joins this list.
const settingsAbout = [/features\/settings\/screens2?\/about\.ts$/];
// The generated screens' navigation icons (X-settings-registry-slim), loaded after the navigation
// shows like the connection screens' (`Screen.iconLoad`); every first view stops at them.
const settingsIcons = [/features\/settings\/screens\/icons\.ts$/];
const settingsFirstView = [
	...settingsOnDemand,
	...settingsIcons,
	...insightsDigest,
	...settingsEdit,
	...settingsConnections,
	...settingsSecurity,
	...settingsSecurityLater,
	...settingsAboutParts,
	...securityLoader,
	...settingsLogging,
	...settingsLoggingFiles,
	...settingsMessagesLeftOut,
	...settingsBodies,
	...settingsExtras
];
const settingsSectionView = own('/settings/[section]', { onDemand: settingsFirstView });
const settingsShellView = own('/settings/[section]', {
	onDemand: [...settingsFirstView, ...settingsFormChunk, ...settingsAbout]
});
const settingsIconsLazy = requireMeasuredJs(
	'Settings screen icons (on demand)',
	onDemandOf('/settings/[section]', settingsIcons)
);
const settingsAboutLazy = requireMeasuredJs(
	'Settings screen purposes (on demand)',
	onDemandOf('/settings/[section]', settingsAbout)
);
const settingsLoggingMap = requireMeasuredJs(
	'Settings logging messages (on demand)',
	loggingMapFiles
);
const settingsLoggingOutputMap = requireMeasuredJs(
	'Settings logging output messages (on demand)',
	loggingOutputMapFiles
);
const settingsLoggingLazy = requireMeasuredJs(
	'Settings logging parts (on demand)',
	onDemandOf('/settings/[section]', settingsLogging).filter(
		(f) =>
			!onDemandOf('/settings/[section]', settingsOnDemand).includes(f) &&
			!onDemandOf('/settings/[section]', settingsLoggingFiles).includes(f)
	)
);
const settingsLoggingFilesLazy = requireMeasuredJs(
	'Settings logging files and preview (on demand)',
	onDemandOf('/settings/[section]', settingsLoggingFiles).filter(
		(f) => !onDemandOf('/settings/[section]', settingsOnDemand).includes(f)
	)
);
const settingsSecurityLazy = requireMeasuredJs('Settings security parts (on demand)', [
	...new Set(
		Object.keys(manifest)
			.filter((key) => settingsSecurity.some((re) => re.test(key)))
			.flatMap((key) =>
				ownChunk(key, [...settingsOnDemand, ...settingsEdit], false, settingsShellView)
			)
	)
]);
// Never leave out code a page loads statically: every body or extra the patterns match must be a
// dynamic import only (`load`, `extra`), or its regex would hide first-view code from the gates.
const bodyKeys = Object.keys(manifest).filter((key) => settingsBodies.some((re) => re.test(key)));
const extraKeys = Object.keys(manifest).filter((key) => settingsExtras.some((re) => re.test(key)));
// The registry's lazy purposes and icons (X-settings-registry-slim) are left out the same way.
const registryLazyKeys = Object.keys(manifest).filter((key) =>
	[...settingsAbout, ...settingsIcons].some((re) => re.test(key))
);
const securityBody = /features\/settings\/security\/SecurityScreen\.svelte$/;
const aboutBody = /features\/settings\/about\/AboutScreen\.svelte$/;
const staticHits = Object.entries(manifest).flatMap(([from, chunk]) =>
	(chunk.imports ?? [])
		.filter(
			(key) => bodyKeys.includes(key) || extraKeys.includes(key) || registryLazyKeys.includes(key)
		)
		// Security's later parts load only from its body, so their imports of that body alone are
		// of code already loaded; any other body they import still trips this.
		.filter((key) => !(settingsSecurityLater.some((re) => re.test(from)) && securityBody.test(key)))
		// The same for About's parts: they load from its body, whose chunk holds what they share.
		.filter((key) => !(settingsAboutParts.some((re) => re.test(from)) && aboutBody.test(key)))
		.map((key) => `${from} -> ${key}`)
);
if (staticHits.length)
	throw new Error(
		`Settings: a screen body, extra, purpose or icon module is imported statically, so it isn't on demand:\n  ${staticHits.join('\n  ')}`
	);
// The same for Security's later parts: they load by dynamic import after mount, so nothing may
// import them statically, or their code would leave the first-view gates unmeasured.
const laterKeys = Object.keys(manifest).filter((key) =>
	[...settingsSecurityLater, ...settingsAboutParts].some((re) => re.test(key))
);
const laterStatic = Object.entries(manifest).flatMap(([from, chunk]) =>
	(chunk.imports ?? []).filter((key) => laterKeys.includes(key)).map((key) => `${from} -> ${key}`)
);
if (laterStatic.length)
	throw new Error(
		`Settings: a Security part loaded on request is imported statically:\n  ${laterStatic.join('\n  ')}`
	);
// Appearance's theme picker (opened with "Browse themes", with themes.json) and its custom theme
// editor (opened from the picker, with the build's theme compiler) load on interaction, so a first
// view stops at them; each has its own entry below.
const APPEARANCE = 'src/lib/features/appearance';
const appearancePicker = [new RegExp(`${APPEARANCE}/ThemePicker\\.svelte$`)];
const appearanceEditor = [new RegExp(`${APPEARANCE}/ThemeEditor\\.svelte$`)];
// The 18+ dialog (Bits UI overlays) loads with the first turn-on of Show adult content.
const appearanceAdult = [/features\/filters\/flows\/AdultConfirmDialog\.svelte$/];
const appearanceLater = [...appearancePicker, ...appearanceEditor, ...appearanceAdult];
const settingsHandBuilt = bodyKeys.map((key) => ({
	name: key
		.split('/')
		.at(-1)
		.replace(/\.svelte$/, ''),
	files: requireMeasuredJs(`Settings ${key} (first view)`, [
		...new Set([
			...settingsShellView,
			...ownChunk(
				key,
				[
					...settingsOnDemand,
					...settingsEdit,
					...settingsConnections,
					...settingsSecurity,
					...settingsSecurityLater,
					...settingsAboutParts,
					...settingsLogging,
					...settingsLoggingFiles,
					...insightsDigest,
					...mapsLeftOut(bodyMaps[mapName(key)] ?? [largestMap]),
					...settingsBodyInteractions,
					...appearanceLater
				],
				false,
				settingsShellView
			)
		])
	])
}));
// What Security's later parts add to its first view (the body is loaded when they load).
const securityFirstView =
	settingsHandBuilt.find(({ name }) => name === 'SecurityScreen')?.files ?? [];
const settingsSecurityLaterLazy = requireMeasuredJs(
	'Settings security keys and proxy (on demand)',
	[
		...new Set(
			Object.keys(manifest)
				.filter((key) => settingsSecurityLater.some((re) => re.test(key)))
				.flatMap((key) =>
					ownChunk(key, [...settingsOnDemand, ...settingsEdit, ...settingsSecurity], false, [
						...settingsShellView,
						...securityFirstView
					])
				)
		)
	]
);
// What About's parts add to its first view (the body is loaded when they load).
const aboutFirstView = settingsHandBuilt.find(({ name }) => name === 'AboutScreen')?.files ?? [];
const settingsAboutPartsLazy = requireMeasuredJs('Settings about parts (on demand)', [
	...new Set(
		Object.keys(manifest)
			.filter(
				(key) =>
					settingsAboutParts.some((re) => re.test(key)) &&
					!settingsAboutModules.some((re) => re.test(key))
			)
			.flatMap((key) =>
				ownChunk(key, [...settingsOnDemand, ...settingsEdit], false, [
					...settingsShellView,
					...aboutFirstView
				])
			)
	)
]);
// The icon and the purpose line load from every Settings page, not from About's body, so what they
// share with About's body is measured here too (beyond the shell only).
const settingsAboutModulesLazy = requireMeasuredJs('Settings about icon and purpose (on demand)', [
	...new Set(
		Object.keys(manifest)
			.filter((key) => settingsAboutModules.some((re) => re.test(key)))
			.flatMap((key) =>
				ownChunk(key, [...settingsOnDemand, ...settingsEdit], false, settingsShellView)
			)
	)
]);
const appearanceBody = bodyKeys.find((key) => key.startsWith(`${APPEARANCE}/`));
const appearanceFirst = appearanceBody
	? ownChunk(
			appearanceBody,
			[...settingsOnDemand, ...settingsEdit, ...settingsMessagesLeftOut, ...appearanceLater],
			false,
			settingsShellView
		)
	: [];
const appearanceKey = (re) => Object.keys(manifest).find((key) => re.test(key));
const pickerKey = appearanceKey(appearancePicker[0]);
const editorKey = appearanceKey(appearanceEditor[0]);
const settingsPickerLazy = requireMeasuredJs(
	'Appearance theme picker (on demand)',
	pickerKey
		? ownChunk(pickerKey, appearanceEditor, false, [...settingsShellView, ...appearanceFirst])
		: []
);
const adultKey = appearanceKey(appearanceAdult[0]);
const settingsAdultLazy = requireMeasuredJs(
	'Appearance 18+ dialog (on demand)',
	adultKey ? ownChunk(adultKey, [], false, [...settingsShellView, ...appearanceFirst]) : []
);
const settingsEditorLazy = requireMeasuredJs(
	'Appearance theme editor (on demand)',
	editorKey ? ownChunk(editorKey, [], false, [...settingsShellView, ...appearanceFirst]) : []
);
const settingsFormFiles = [settingsFormChunk[0], languageForm[0]].flatMap((re) => {
	const key = Object.keys(manifest).find((k) => re.test(k));
	return key ? ownChunk(key, [...settingsOnDemand, ...settingsEdit], false) : [];
});
// The closure stops at the shell's section form and Language & region's form (`settingsFormFiles`):
// both are already counted in their screens' first views, so extras are only what loads after them.
const settingsExtrasLazy = requireMeasuredJs('Settings screen extras (on demand)', [
	...new Set([
		...extraKeys.flatMap((key) =>
			ownChunk(key, [...settingsOnDemand, ...settingsEdit], false, settingsSectionView)
		),
		...bodyKeys.flatMap((key) =>
			ownChunk(
				key,
				[...settingsOnDemand, ...settingsEdit, ...settingsMessages, ...settingsBodyInteractions],
				true,
				settingsFormFiles
			)
		)
	])
]);

// What another route can still add to /search's first load (runner, 2026-10-24):
// - The route table: one import and one dictionary entry per route, about 27 B gzipped. The budget
//   allows 1.26 KB (measured with only / and /search) plus 40 B a route, so a route that adds
//   more fails here.
// - The vendor chunk holds only modules /search reaches (the stray check above), but it is shared,
//   so another route that uses more of those modules' exports keeps them in it. Measured 31.09 KB
//   with S-settings-reconcile (2026-10-30): the Settings form engine and Keyboard's screen use more of
//   the Svelte runtime than any earlier route (30.42 KB with every route before them, 30.81 KB with
//   848e10a0's Settings). The 31.1 KB budget is the ceiling: a route that passes it goes to the owner,
//   and only a new dependency of /search's own raises it, on purpose.
const routeCount = existsSync('.svelte-kit/generated/client-optimized/app.js')
	? (readFileSync('.svelte-kit/generated/client-optimized/app.js', 'utf8')
			.match(/dictionary = (\{[\s\S]*?\});/)?.[1]
			.match(/"\/[^"]*":/g)?.length ?? 0)
	: 0;
const vendor = Object.values(manifest).find((chunk) => chunk.name === 'vendor');

// Search results (S-search-2): the panel, its row and its sheets, loaded on a query or the first
// interaction, plus what it loads with the first results (the row actions, the j/k commands and the
// key registry). Opening a result (the drawer, its commands), copying a magnet and the More menu load
// on demand: the Detail entries gate the first two, and the menu (Bits UI) is the shell's too.
const RESULTS = [
	'src/lib/features/search/results/ResultsPanel.svelte',
	'src/lib/features/search/results/results.css',
	// row.css shares a chunk with Alerts' detail page, so it has no key of its own.
	...Object.keys(manifest).filter((k) =>
		(manifest[k].css ?? []).some((c) => /\/row\.[^/]*\.css$/.test(c))
	)
];
// Table mode, its column chooser and the bulk bar load on demand (S-search-5): their own entries.
const tableOnDemand = [/search\/table\/(ResultsTable|ColumnChooser)\.svelte$/];
const bulkOnDemand = [/search\/bulk\/(BulkBar|RowPick)\.svelte$/, /search\/bulk\/ops\.ts$/];
const recoverOnDemand = [/search\/results\/Recover\.svelte$/];
const indexStateOnDemand = [/search\/results\/IndexState\.svelte$/];
const resultsOnDemand = [
	/features\/detail\//,
	/features\/search\/(live|cards)\//,
	/^chunk:magnet$/,
	/^chunk:Menu$/,
	...tableOnDemand,
	...bulkOnDemand,
	...recoverOnDemand,
	...indexStateOnDemand
];
// What the view has loaded by the time the panel does (runner, 2026-10-24): the first load and the
// bar's lazy part, which a URL with a query engages at once. Not the shell's Chrome: on a pasted
// link it loads only with the first interaction, so the panel really does fetch what they share.
const viewOf = (...parts) => [
	...new Set(parts.flatMap(([key, onDemand]) => ownChunk(key, onDemand)))
];
const engagedSearch = viewOf([
	'src/lib/features/search/query/Live.svelte',
	[/query\/(FilterStrip|SyntaxPopover)\.svelte$/, /features\/search\/assist\//]
]);
const resultsLazy = [
	...new Set(RESULTS.flatMap((k) => ownChunk(k, resultsOnDemand, false, engagedSearch)))
];
// The facet rail (S-search-2b): its host loads with the results, and the rail with it where the
// rail shows (x-large); below that, on the first Filters press, which also brings the sheet (on
// demand). Measured beyond the engaged view and the results panel it loads with.
const FACETS = [
	'src/lib/features/search/facets/FacetsHost.svelte',
	'src/lib/features/search/facets/FacetRail.svelte'
];
// S-search-recover-ui: "Removing X gives N" and the index-state banners load when /search/state or an
// empty answer needs them, beyond the engaged view and the results panel.
const recoverLazy = [
	...new Set(
		ownChunk('src/lib/features/search/results/Recover.svelte', [], false, [
			...engagedSearch,
			...resultsLazy
		])
	)
];
const indexStateLazy = [
	...new Set(
		ownChunk('src/lib/features/search/results/IndexState.svelte', [], false, [
			...engagedSearch,
			...resultsLazy
		])
	)
];
const facetsOnDemand = [/ui\/overlays\/Sheet\.svelte$/];
const facetsLazy = [
	...new Set(
		FACETS.flatMap((k) => ownChunk(k, facetsOnDemand, false, [...engagedSearch, ...resultsLazy]))
	)
];
// The results toolbar (S-search-3): loads with the results and shows once there is a query.
// Save search's dialog (the alerts', with Bits UI's overlay code) loads on the first press.
const TOOLBAR = [
	'src/lib/features/search/toolbar/ResultsToolbar.svelte',
	'src/lib/features/search/toolbar/toolbar.css'
];
const toolbarOnDemand = [/features\/alerts\/SaveSearchDialog\.svelte$/];
const toolbarLazy = [
	...new Set(
		TOOLBAR.flatMap((k) => ownChunk(k, toolbarOnDemand, false, [...engagedSearch, ...resultsLazy]))
	)
];

// Table mode (S-search-5): the table with its row and header code; the column chooser loads on its
// first open. The bulk bar loads with the first selected row. Both beyond the results panel.
const tableLazy = [
	...new Set(
		ownChunk(
			'src/lib/features/search/table/ResultsTable.svelte',
			[/search\/table\/ColumnChooser\.svelte$/],
			false,
			[...engagedSearch, ...resultsLazy]
		)
	)
];
const chooserLazy = [
	...new Set(
		ownChunk('src/lib/features/search/table/ColumnChooser.svelte', [], false, [
			...engagedSearch,
			...resultsLazy,
			...tableLazy
		])
	)
];
const pickLazy = [
	...new Set(
		ownChunk('src/lib/features/search/bulk/RowPick.svelte', [], false, [
			...engagedSearch,
			...resultsLazy
		])
	)
];
const bulkLazy = [
	...new Set(
		ownChunk('src/lib/features/search/bulk/BulkBar.svelte', [/^chunk:magnet$/], false, [
			...engagedSearch,
			...resultsLazy
		])
	)
];

// Search assist (S-search-4): autocomplete loads on the bar's first focus or keystroke (with the
// recent-query store and its stylesheet); the natural-phrasing preview and the Builder (with its
// selects and inputs) load on their buttons. Each is measured beyond the view it loads into.
const ASSIST = [
	'src/lib/features/search/assist/Assist.svelte',
	'src/lib/features/search/assist/recent.ts',
	'src/lib/features/search/assist/assist.css'
];
const assistLazy = [...new Set(ASSIST.flatMap((k) => ownChunk(k, [], false, engagedSearch)))];
const naturalLazy = ownChunk('src/lib/features/search/assist/Natural.svelte', [], false, [
	...engagedSearch,
	...assistLazy
]);
const builderLazy = ownChunk('src/lib/features/search/assist/Builder.svelte', [], false, [
	...engagedSearch,
	...assistLazy
]);

// The live banner (S-search-6) loads once the results are ready; Cards loads when the density is
// Cards. Each is measured beyond the views it loads into.
const liveLazy = [
	...new Set(
		[
			'src/lib/features/search/live/live.svelte.ts',
			'src/lib/features/search/live/LiveHost.svelte'
		].flatMap((k) => ownChunk(k, [], false, [...engagedSearch, ...resultsLazy]))
	)
];
const cardsLazy = [
	...new Set(
		[
			'src/lib/features/search/live/extras.ts',
			'src/lib/features/search/cards/CardList.svelte',
			'src/lib/features/search/cards/Card.svelte',
			'src/lib/features/search/cards/cards.ts',
			'src/lib/features/search/cards/cards.css'
		].flatMap((k) => ownChunk(k, [], false, [...engagedSearch, ...resultsLazy]))
	)
];

// Never pass by measuring nothing: a renamed or moved file must fail here, not drop the entry.
if (Object.keys(manifest).length && !resultsLazy.some((f) => f.endsWith('.js')))
	throw new Error('Search results (lazy): no chunk found for ResultsPanel.svelte in the manifest');
if (Object.keys(manifest).length && !facetsLazy.some((f) => f.endsWith('.js')))
	throw new Error('Facet rail (lazy): no chunk found for the facet rail in the manifest');
for (const [name, files] of [
	['Search assist', assistLazy],
	['Natural phrasing', naturalLazy],
	['Builder', builderLazy]
])
	if (Object.keys(manifest).length && !files.some((f) => f.endsWith('.js')))
		throw new Error(`${name} (on demand): no chunk found in the manifest`);
if (Object.keys(manifest).length && !toolbarLazy.some((f) => f.endsWith('.js')))
	throw new Error('Results toolbar (lazy): no chunk found for the toolbar in the manifest');

// Alerts: the save and delete dialogs load when one is opened, and bring Bits UI's overlay code.
const alertDialogs = [/features\/alerts\/(SaveSearch|DeleteAlert)Dialog\.svelte$/];
// The alert page's j/k (and the key registry) load after mount, as Detail's commands do.
const alertOnDemand = [...alertDialogs, /features\/alerts\/commands\.ts$/];

export default [
	entry('Search route JS (initial)', search.js, '45 KB'),
	entry('CSS', search.css, '20 KB'),
	...(appEntry
		? [entry('Route table (app.js)', [appEntry], `${(1.26 + 0.04 * routeCount).toFixed(2)} KB`)]
		: []),
	...(vendor ? [entry('Search vendor chunk', [`build/${vendor.file}`], '31.1 KB')] : []),
	// Total first load: html + search JS + CSS + the Latin font (fetched at first layout, not preloaded).
	entry(
		'Total first load',
		['build/index.html', ...search.js, ...search.css, ...firstPaintFonts],
		'100 KB'
	),
	// Latin subset of Atkinson Hyperlegible Next, fetched at first layout; a preload costs LCP (§10.1).
	entry('Fonts on first paint', firstPaintFonts, '35 KB'),
	// Mono loads lazily and is never preloaded (§10.1).
	entry('Mono font (lazy)', monoFonts, '25 KB'),
	// Route budgets (runner decision, owner's §10.1 headroom rules): each route's own JS + CSS.
	// Measured 25.96 KB with its lazy chunks (the screen, both layouts, labels, formatters, and the
	// Svelte runtime only this route uses since #132 moved it out of /search's first load); 28.06 KB
	// after #139 and #154 regrouped the shared chunks it pulls in (Button, Badge, transitions,
	// messages). Limit: measured + 10%.
	entry('Compare route (own JS + CSS)', own('/compare'), '31 KB'),
	// Measured 42.8 KB: over the 15 KB target because the screen is a virtualised list (12 KB with
	// the Svelte runtime only lazy routes use), an 11 KB page with both languages' messages, the key
	// registry, the API client and the formatters. Limit: the §10.1 cap for route JS.
	entry('Discover route (own JS + CSS)', own('/discover'), '45 KB'),
	// Logs (S-logs-1): the live log's own JS + CSS, with the virtual list, the stream client and
	// both languages' messages. Limit = measured + 10% (measured 35.97 KB).
	// Turn off now's privacy requests (S-logs-2) load on the click: gated with Logging's parts.
	entry('Logs route (own JS + CSS)', own('/logs', { onDemand: logsLater }), '39.6 KB'),
	// Owner ruling, 2026-10-30 (S-settings-reconcile): with Settings a production route, Rolldown
	// re-splits shared chunks (Toggle, format/bytes, Svelte's input bindings) these entries load;
	// the same modules cost more chunk overhead. Alerts 29 → 29.1 KB, Alert 45 → 45.8 KB, Detail
	// sections 33.9 → 34.5 KB.
	// Raised by the runner (§2.26): +69 B from the chunk re-split when /logs was added (S-logs-1).
	entry('Alerts route (own JS + CSS)', own('/alerts', { onDemand: alertDialogs }), '29.2 KB'),
	// Alerts: first view 26.11 KB (limit = measured + 10%). Alert: 41.2 KB, so 45 KB (the §10.1 cap) holds:
	// the virtual list for long hit lists (VirtualList, its virtualiser, the hit row and composition bar).
	// Raised from 45.8 KB by the runner under owner-decisions §2.26 (docs/decisions/2026-10-31-owner-delegation.md).
	// Raised by the runner (§2.26): +86 B from the chunk re-split when /logs was added (S-logs-1).
	entry('Alert route (own JS + CSS)', own('/alerts/[id]', { onDemand: alertOnDemand }), '46 KB'),
	entry(
		'Settings overview (own JS + CSS)',
		own('/settings', { onDemand: [...settingsFirstView, ...settingsAbout] }),
		'45 KB'
	),
	entry('Settings section (own JS + CSS)', settingsSectionView, '45 KB'),
	// The engine's on-demand code on a Settings screen, still gated: 34.34 KB with S-settings-1's
	// screen (limit = measured + 10%). The production screen now uses it: missing chunks fail.
	entry('Settings controls (on demand)', settingsLazy, '38 KB'),
	entry('Settings edit chunk (on demand)', settingsEditLazy, '11.3 KB'),
	entry('Insights digest preview (on demand)', insightsDigestLazy, '1.9 KB'),
	entry('Insights print layout (on demand)', insightsPrintLazy, '1.62 KB'),
	entry('Settings messages (on demand)', settingsMessagesLazy, '4 KB'),
	entry('Settings connection messages (on demand)', settingsConnectionMaps, '4.3 KB'),
	entry('Settings connection screens (on demand)', settingsConnectionsLazy, '14.2 KB'),
	// Security's password dialog, lock-out banner and navigation icon: measured + 10%.
	entry('Settings security parts (on demand)', settingsSecurityLazy, '26 KB'),
	// Security's key manager, proxy section and reveal dialog: measured + 10%.
	entry('Settings security keys and proxy (on demand)', settingsSecurityLaterLazy, '33.1 KB'),
	// About's diagnostics block and licence list (S-about): LIMIT_ABOUT_PARTS
	entry('Settings about parts (on demand)', settingsAboutPartsLazy, '3.44 KB'),
	entry('Settings about icon and purpose (on demand)', settingsAboutModulesLazy, '0.39 KB'),
	// Logging (S-logs-2): its server labels, and the event list, data kinds and privacy dialogs.
	// Measured 0.78 KB and 15.48 KB at first (limits = measured + 10%); 16.48 KB after the review
	// round (the unreadable-state banner, closing dialogs on leave).
	entry('Settings logging messages (on demand)', settingsLoggingMap, '0.86 KB'),
	// The outputs' labels and help (S-logs-2b), split from Logging's map (runner, §2.26). Measured
	// 0.70 KB; limit = measured + 10%.
	entry('Settings logging output messages (on demand)', settingsLoggingOutputMap, '0.77 KB'),
	entry('Settings logging parts (on demand)', settingsLoggingLazy, '17.1 KB'),
	// S-logs-2b: the outputs, the preview, the log files and the Danger zone, measured 9.81 KB.
	entry('Settings logging files and preview (on demand)', settingsLoggingFilesLazy, '10.8 KB'),
	// Each hand-built screen's first view (the route without the form, plus its body), against the
	// 45 KB route cap (§10.1).
	// A new registry entry adds ≈ 65–85 B to every one of these and to "Settings section" (its
	// title ≈ 25 B, the entry with its icon and body loaders ≈ 40–60 B; the note at the top of
	// shell/screens.ts). Before X-settings-registry-slim each also carried its purpose and an eager
	// icon; those now load after the first view, so every view has ≥ 1.3 KB headroom.
	...settingsHandBuilt.map(({ name, files }) =>
		entry(`Settings ${name} (first view)`, files, '45 KB')
	),
	// Appearance's theme picker and custom theme editor, each beyond the screen's first view.
	entry('Appearance theme picker (on demand)', settingsPickerLazy, '5.5 KB'),
	entry('Appearance 18+ dialog (on demand)', settingsAdultLazy, '20.6 KB'),
	entry('Appearance theme editor (on demand)', settingsEditorLazy, '13 KB'),
	// General's export panel beyond the section's first view, and what a hand-built body loads on an
	// interaction (Keyboard's shortcut sheet and toggle): limit = measured + 10%.
	entry('Settings screen extras (on demand)', settingsExtrasLazy, '25 KB'),
	// The registry's purpose lines and notices, and the generated screens' navigation icons
	// (X-settings-registry-slim): measured 0.87 KB and 1.5 KB, limits = measured + 10%.
	entry('Settings screen purposes (on demand)', settingsAboutLazy, '0.96 KB'),
	entry('Settings screen icons (on demand)', settingsIconsLazy, '1.66 KB'),
	// The dialogs' on-demand code (Bits UI overlays, Dialog, both dialogs), still gated.
	entry(
		'Alert dialogs (on demand)',
		[
			...new Set([
				...onDemandOf('/alerts', alertDialogs),
				...onDemandOf('/alerts/[id]', alertDialogs)
			])
		],
		'27 KB'
	),
	// Limits are measured + 10%. Over the 15 KB target (§10.1) because the first view carries the
	// Drawer primitive, ~40 messages, the Svelte runtime and key registry only this view uses so far, and
	// the format and API helpers; the tree and virtual list are in the on-demand entry.
	// Detail (S-detail): the first view is the header and the summary. Contents (composition and the
	// file tree with its virtual list), Parsed and the error state are separate chunks that load as
	// their section scrolls near or an error shows, each gated by the on-demand entry.
	// Beyond Live's mount set (runner, 2026-10-24: 41.5 KB, under the 45 KB per-view cap). VirtualList
	// and its virtualiser are 12 KB of it, the panel 5.2 KB, the row 3.9 KB, the row actions 1.9 KB;
	// the rest is what the shell's Chrome would bring too (the key registry, the event client).
	// runner, 2026-10-31, S-search-5: +1.5 KB for the view switch, selection store, `x` key and announce;
	// still under the 45 KB per-view cap (§10.1).
	// Raised by the runner (§2.26): +92 B from the chunk re-split when /logs was added (S-logs-1).
	entry('Search results (lazy)', resultsLazy, '43.3 KB'),
	// Over the 15 KB target (§10.1; runner, 2026-10-24: 16.8 KB). The self-review added Release year,
	// the unavailable and stale-lease states, the staged-count announcement and day-snapped brushes.
	// The FacetRail chunk is 9.9 KB, the host 3.1 KB, facets.css 0.9 KB, Checkbox and input 1.1 KB.
	entry('Search recovery counts (on demand)', recoverLazy, '0.84 KB'),
	entry('Search index-state banners (on demand)', indexStateLazy, '3.3 KB'),
	entry('Facet rail (lazy)', facetsLazy, '16.8 KB'),
	// runner, 2026-10-31, S-search-5: +0.2 KB for the View and Select controls and their messages;
	// still under the 45 KB per-view cap (§10.1).
	// runner, 2026-10-31, X-build-stable-name (§2.26): SHA in version literal varies gzip ±few B; was 4,593–4,603 B on main
	entry('Results toolbar (lazy)', toolbarLazy, '4.65 KB'),
	entry('Search live banner (lazy)', liveLazy, '3.02 KB'),
	// Raised by the runner (§2.26): +21 B from the chunk re-split when /logs was added (S-logs-1).
	entry('Search cards (lazy)', cardsLazy, '8.3 KB'),
	entry('Search table mode (on demand)', tableLazy, '6.5 KB'),
	entry('Search column chooser (on demand)', chooserLazy, '3.4 KB'),
	entry('Search bulk bar (on demand)', bulkLazy, '3.7 KB'),
	entry('Search row checkboxes (on demand)', pickLazy, '1.7 KB'),
	entry('Search assist (on demand)', assistLazy, '6.59 KB'),
	entry('Natural phrasing (on demand)', naturalLazy, '1.64 KB'),
	entry('Builder (on demand)', builderLazy, '5.69 KB'),
	// 29 KB (runner, 2026-10-30, L3–L5 landing): visibility revalidation (L3) subscribes the drawer to
	// settings events, so the event client (2.1 KB) now loads with it until the shell loads it too.
	entry('Detail drawer (own JS + CSS)', ownChunk(DRAWER, detailLazy), '29 KB'),
	// Raised by the runner (§2.26): +~40 B gzip noise from re-hashed chunk names, no code change in the route (#275).
	// Raised by the runner (§2.26): +35 B from the chunk re-split when /logs was added (S-logs-1).
	entry('/t/[hash] route (own JS + CSS)', own('/t/[hash]', { onDemand: detailLazy }), '22.6 KB'),
	entry(
		'Detail sections (on demand)',
		[...new Set([...ownChunk(DRAWER, detailLazy, true), ...onDemandOf('/t/[hash]', detailLazy)])],
		'34.5 KB' // owner ruling, 2026-10-30: see the Alerts entries
	),
	// The four stage pages (S-insights-2) are one route file and one lazy component, so one entry
	// measures them all. uPlot loads with the first chart, after first paint, and has its own entry.
	entry(
		'Insights stage pages (own JS + CSS)',
		own('/insights/[stage]', { onDemand: stageLazy }),
		'39.7 KB'
	),
	entry(
		'Insights stage charts (uPlot, on demand)',
		onDemandOf('/insights/[stage]', stageLazy),
		'24.2 KB'
	)
	// Placeholders for later slices. Fill `path` from the manifest when the chunk exists:
	//   Insights chunk (uPlot) <= 45 KB
];
