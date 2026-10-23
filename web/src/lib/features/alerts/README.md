# features/alerts

The Alerts list (`/alerts`), one alert (`/alerts/<id>`) and the dialogs that create and delete a saved search. Architecture §10.8; decisions in `docs/decisions/2026-10-23-S-alerts.md`.

## For S-search-3: the "Save search" dialog

`SaveSearchDialog.svelte` is the dialog behind the results toolbar's **Save search**. Load it with `import()` the first time the button is used: it brings Bits UI's overlay code (about 15–20 KB gzipped) and must stay out of the Search route's first chunk.

```svelte
<script lang="ts">
	let Dialog = $state<typeof import('$lib/features/alerts/SaveSearchDialog.svelte').default>();
	let open = $state(false);

	async function save() {
		Dialog ??= (await import('$lib/features/alerts/SaveSearchDialog.svelte')).default;
		open = true;
	}
</script>

<Button onclick={save}>Save search</Button>
{#if Dialog}
	<Dialog
		bind:open
		q={query}
		{sort}
		onSaved={(saved) => {
			/* optional */
		}}
	/>
{/if}
```

| Prop                                     | Meaning                                                                                                                                                |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `open = $bindable(false)`                | Open state. Opening resets the form: empty name, `q` in the query field, "In the app" on, "RSS feed" off.                                              |
| `q?: string`                             | Prefills the query (the canonical text Search shows). Empty or absent: the saved search matches every new torrent.                                     |
| `sort?: string \| null`                  | Search's sort, passed to the server as given. `null`/absent saves none (relevance).                                                                    |
| `onSaved?: (s: SavedSearchView) => void` | Decides what happens next. Without it the dialog opens the new alert at `/alerts/<id>`. With it, the dialog only toasts "Saved '<name>'" and calls it. |

Types come from `$lib/api/schema` (`components['schemas']['SavedSearchView']`). The dialog calls `POST /saved-searches` itself and maps every error (`query_syntax` onto the Query field with the span selected, `invalid_value` onto the field the server names, `too_many_saved_searches` and the rest into a form-level message). The opener keeps focus on close: the primitive restores it.

The dialog has no filter or AST input: Search's current query text is the whole contract (`q`), and the server parses it again.

## Files

- `api.ts`: the endpoints, the types, and the only places a link is built from data (`/alerts/<id>`, `/t/<40 hex>`, `/search?q=…`, `/api/v1/feeds/<id>`).
- `list.svelte.ts`, `detail.svelte.ts`: the pages' state (loading, paging, the read mark, the banner), tested without a DOM.
- `divider.ts`: where the "New since …" divider goes (a pure function of the server's `read` flags).
- `throttle.ts`: the 10 s window for the polite live region.
- `errors.ts`: save failures → field or form message.
- `AlertsListPage.svelte`, `AlertDetailPage.svelte`: the pages. `AlertHitRow.svelte`: an interim result row (`TODO(S-search-2)`).
- `SaveSearchDialog.svelte`, `DeleteAlertDialog.svelte`: loaded with `import()` on first use. `PauseToggle.svelte`, `ChannelBadges.svelte`.
