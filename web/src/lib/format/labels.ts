// One label formatter per field (X-labels; architecture §10.4, §10.9, docs/ui-glossary.md): what a
// screen shows for a value the server sends as a code. Words come from the glossary where it names
// the term, and from en and zh messages otherwise; codec, HDR and release-source names are proper
// names, the same in every language. A value with no label shows as sent, never as an empty label,
// and only what's shown changes: a typed query and a URL keep the codes. Each table is its own
// constant, so a screen that imports one formatter doesn't carry the others.
//
// Each field's formatter is its own module (`label-*.ts`), and screens import that module, never
// this one or `$lib/format`: a table then loads only with the screens that show it. This file
// gathers them for the tests and as the index.
export * from './label-field';
export * from './label-release';
export * from './label-source';
export * from './label-theme';
export * from './label-type';
