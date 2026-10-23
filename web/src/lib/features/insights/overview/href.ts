// `resolve()` is typed by route id; our links are built strings (query, range), so they are cast to
// one id. There is no base path, so it returns the path unchanged. The lint rule wants the call
// right at the link, so each template calls `resolve(path as Route)` itself.
export type Route = '/insights';
