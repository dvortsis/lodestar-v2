# Product refinements during the CR-0 baseline

Authority: owner-approved product scope in [DHT implementation plan §13](../plan/dht-crawler-implementation-plan-2026-10-25.md#13-product-work-during-the-soak). These changes do not advance crawler experiments, change consent/defaults or modify the running baseline.

- Detail cache presence is not permission to display metadata. Every open revalidates the detail endpoint before revealing either detail or associated search-hit metadata. Content-settings events/resync and session transitions invalidate caches and asynchronous generations. Hash-based magnet actions revalidate for the same reason.
- `/welcome` is a small authenticated completion screen, with Search and Settings links. The existing `/auth` check handles direct visits/refresh; failure shows retry rather than claiming success. It performs no settings writes.
- Search retains its lazy detail host through shallow history closure. Focus returns to a connected invoking result or the query editor, without overriding focus deliberately moved outside the drawer. Full-page detail remains independent.
- `/search?browse=1` explicitly permits an empty match-all search using the existing AST. A typed query takes precedence, including syntax errors. Results and facets include the flag in frontend request identity; the HTTP contract is unchanged. Filters and sort preserve browsing, while Clear all/reset removes it and preserves density.

Execution and outstanding gates are recorded in [the product work log](../plan/2026-10-26-soak-product-execution.md). Prepared source is not validated acceptance; heavy checks wait for the actual collector completion.
