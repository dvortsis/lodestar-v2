# features/auth

First-run setup (`/setup`), sign-in (`/login`), sign-out and the session-expiry redirect. Architecture §12, §10.10.10 steps 1–2, §16 "M2 implementation decisions (API and auth)"; decisions in `docs/decisions/2026-10-22-M-auth.md`.

## For the shell (W9)

Import from `session.svelte.ts`. It pulls in no components or brand assets, so it's safe on the first-load path.

| Export                                                                           | Use                                                                                                                                                                                                                                                                         |
| -------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `installAuthHandlers(): () => void`                                              | Call once from the root layout (`onMount(installAuthHandlers)`; it returns the uninstall). Any 401 `login_required` then goes to `/login?next=<current path and query>`, or to `/setup` when the server says `setup_required`. Uses W5's `onLoginRequired` / `onSessionOk`. |
| `currentSession(): 'unknown' \| 'signed_in' \| 'signed_out' \| 'setup_required'` | Reactive. Hide the top bar's account menu unless `signed_in`, for example.                                                                                                                                                                                                  |
| `logout(): Promise<void>`                                                        | `POST /logout`, then `/login`. Rejects (and stays put) if the server can't be reached.                                                                                                                                                                                      |
| `LogoutButton.svelte`                                                            | "Sign out" with the busy state and an error toast; props `variant` (`quiet` default, `secondary`) and `dense`. Needs the shell's `<Toaster>`.                                                                                                                               |

The sign-in and setup pages are full-page: the shell should render them without the top bar (`page.url.pathname` is `/login` or `/setup`).

## Modules

- `machine.ts`: the forms' state machine (`idle → submitting → success | error | locked`), pure, plus `classify()` from an `ApiError` to a failure kind. The diagram is in the file.
- `form.svelte.ts`: runs the machine for one form; ticks the rate-limit countdown.
- `token.ts`: `parseToken()`, the paste-tolerant setup-token parser.
- `password.ts`: `checkPassword()`: blocks under 10 characters or over 1,024 bytes; otherwise only hints.
- `next.ts`: `safeNext()` (same-origin relative paths only; never back to `/login` or `/setup`) and `loginUrl()`.
- `gate.ts`, `AuthGate.svelte`: the load check (`GET /auth`) that redirects a page that doesn't apply.
- `nav.ts`: every navigation the flows make.
- `errors.ts`: failure → message. Only fixed strings; the server's text is never shown.

## Security properties

- Secrets live only in component state: never in the URL (forms are `method="post"` and handled in script), `localStorage`, `sessionStorage` or a log.
- Errors never echo the token or password; messages are fixed per error code.
- `autocomplete`: `current-password` at sign-in, `new-password` at setup, a fixed hidden `username` for password managers.
- One request at a time: the machine drops a submit while sending or locked, and the button is `aria-disabled` and ignores clicks meanwhile.
- `next` is validated before navigating (no open redirect).
- Every write goes through W5's client (JSON, same-origin cookies).
