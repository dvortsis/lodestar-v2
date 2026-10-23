// Routes that render without the app chrome: the first-run and sign-in screens use the lockup and
// have no navigation or status (§10.19 "App chrome: none"). They also open no event stream.
const BARE = ['/login', '/setup', '/welcome'];

export function isBareRoute(pathname: string): boolean {
	return BARE.some((p) => pathname === p || pathname.startsWith(p + '/'));
}
