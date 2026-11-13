// The one signal the Settings form sends the restart watch: a save answered. The form's
// `send()` (`settings/form/saving.ts`) dispatches this event with the literal inline, importing
// nothing from here, so no restart code joins the form's bundle; `form.test.ts` pins the two
// names together. The watch (`watch.svelte.ts`) listens while it runs. A save sent before the
// watch starts is heard by no one, which is fine: the watch's first ask then comes after it.

export const SETTINGS_SAVED = 'ls-settings-saved';
