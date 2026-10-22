# lib/i18n

Locale selection and the shared messages. How features add messages: `web/README.md` → Messages.

- `locale.svelte.ts`: `initLocale(setting)`, `currentLocale()`, `resolveLocale()`.
- `messages/{en,zh}.json`: shared strings (`common_*`, `glossary_*`).
- Compiled Paraglide output goes to `src/lib/paraglide/` (generated, gitignored).
