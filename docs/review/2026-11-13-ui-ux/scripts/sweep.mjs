// Route sweep: every route × widths × mode. Screenshots + probe + axe per cell.
import { readFileSync, existsSync } from 'node:fs';
import { launch, context, watch, settle, wake, probe, axe, save, shot, OUT } from './lib.mjs';

const FIX = process.env.FIXDIR ?? '/tmp/lodestar-fixture';
const man = JSON.parse(readFileSync(`${FIX}/fixture.json`, 'utf8'));
const byName = (s) => man.torrents.find((t) => t.name.includes(s));
const lantern = man.torrents.filter((t) => t.name.includes('Lantern')).slice(0, 4);
const pack = man.torrents.find((t) => t.name.startsWith('Field Recordings Archive'));

const SETTINGS = ['appearance', 'keyboard', 'language', 'crawler', 'network', 'trackers', 'sources', 'storage', 'search', 'filters', 'blocklists', 'integrations', 'notifications', 'outputs', 'companions', 'insights', 'general', 'security', 'logging', 'jobs', 'updates', 'about'];
const ROUTES = [
	['search-empty', '/search'],
	['search-q', '/search?q=lantern'],
	['search-browse', '/search?browse=1'],
	['search-compact', '/search?q=lantern&density=compact'],
	['search-none', '/search?q=zzzzqqqxxnotfound'],
	['search-badq', '/search?q=size%3A%3E%3Ebanana+%28'],
	['detail', `/t/${lantern[0].hash}`],
	['detail-pack', `/t/${pack.hash}`],
	['detail-missing', `/t/${'f'.repeat(40)}`],
	['compare', `/compare?h=${lantern.map((t) => t.hash).slice(0, 3).join(',')}`],
	['discover', '/discover'],
	['alerts', '/alerts'],
	['alert-1', '/alerts/1'],
	['insights', '/insights'],
	['insights-discover', '/insights/discover'],
	['insights-fetch', '/insights/fetch'],
	['insights-understand', '/insights/understand'],
	['insights-index', '/insights/index'],
	['insights-serve', '/insights/serve'],
	['settings', '/settings'],
	...SETTINGS.map((s) => [`settings-${s}`, `/settings/${s}`]),
	['logs', '/logs'],
	['notfound', '/nope/nothing']
];

const only = process.env.ONLY ? new RegExp(process.env.ONLY) : null;
const CELLS = (process.env.CELLS ?? '320:light,390:light,390:dark,768:light,1024:light,1366:light,1366:dark,1920:light').split(',').map((c) => c.split(':'));

const browser = await launch();
const summary = [];
for (const [w, mode] of CELLS) {
	const width = +w;
	const ctx = await context(browser, { width, height: width < 700 ? 800 : 900, mode, coarse: width < 700 });
	const page = await ctx.newPage();
	const log = watch(page);
	for (const [name, path] of ROUTES) {
		if (only && !only.test(name)) continue;
		const cell = `${name}@${w}-${mode}`;
		if (existsSync(`${OUT}/${cell}.json`) && !process.env.FORCE) continue;
		log.console.length = log.pageErrors.length = log.failed.length = log.http.length = 0;
		try {
			await page.goto(path, { waitUntil: 'domcontentloaded' });
			await settle(page);
			await wake(page);
			await settle(page, 400);
			const p = await probe(page);
			let a = [];
			try {
				a = await axe(page);
			} catch (e) {
				a = [{ id: 'axe-failed', help: String(e).slice(0, 200), n: 0, targets: [] }];
			}
			await shot(page, cell);
			const rec = { cell, path, probe: p, axe: a, log: JSON.parse(JSON.stringify(log)) };
			save(cell, rec);
			summary.push({ cell, hOverflow: p.hOverflow, poke: p.poke.length, small: p.smallCount, tiny: p.tiny.length, clipped: p.clippedCount, axe: a.map((v) => `${v.id}(${v.n})`).join(','), errs: log.console.length + log.pageErrors.length, http: log.http.join(' | ').slice(0, 200) });
			console.log(JSON.stringify(summary.at(-1)));
		} catch (e) {
			console.log(`FAIL ${cell}: ${String(e).slice(0, 300)}`);
		}
	}
	await ctx.close();
}
save('_summary', summary);
await browser.close();
