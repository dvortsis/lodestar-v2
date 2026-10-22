# lib/features

Feature compositions (search, detail, alerts, insights, settings, ...), one folder per feature, each with its own `messages/{en,zh}.json`. May import from `ui/`, `charts/`, `api/`, `state/`, `format/`; `ui/` must never import from here.
