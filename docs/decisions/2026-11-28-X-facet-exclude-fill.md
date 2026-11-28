# X-facet-exclude-fill decisions (#1145, finished in r2)

- **Facet rail (lazy) size limit raised, 18.86 KB → 19.01 KB (runner §2.26 grant, 2026-11-27).** The filled exclude icon grows the chunk from 18,856 B to a measured 18,958 B (`npm run size:ci`); the limit is that size rounded up to the next 64 B (19,008 B), written as 19.01 KB. No other entry changed. This is a performance budget change the runner decides under `docs/owner-decisions.md` §2.26.
