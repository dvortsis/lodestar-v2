//! Lines the code didn't type: the third-party crates' (Libraries, capped
//! at warnings by default) and the first-party ones the follow-up sweep
//! converts. They pass the reloadable filter, which holds each
//! component's level, and are scrubbed before any output sees them: URLs
//! lose their queries, userinfo and passkey-like segments, and runs that
//! look like an info hash or a session secret are hidden.

use std::fmt;

use lodestar_log::Component;
use lodestar_log::url::scrub_text;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use super::format::Untyped;

/// Hands every `tracing` event to the runtime.
#[derive(Debug, Clone, Copy, Default)]
pub struct Forward;

#[derive(Default)]
struct Fields {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            name => self.fields.push((name.to_string(), value.to_string())),
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            name => self.fields.push((name.to_string(), format!("{value:?}"))),
        }
    }
}

/// `event` as an untyped line, scrubbed.
pub fn untyped(event: &Event<'_>) -> Untyped {
    let meta = event.metadata();
    let mut f = Fields::default();
    event.record(&mut f);
    let target = meta.target();
    Untyped {
        level: super::level_of(meta.level()),
        target: target.to_string(),
        component: super::component_of(target)
            .unwrap_or(Component::Libraries)
            .name(),
        message: scrub_text(&f.message),
        fields: f
            .fields
            .into_iter()
            .map(|(k, v)| (k, scrub_text(&v)))
            .collect(),
    }
}

impl<S: Subscriber> Layer<S> for Forward {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if let Some(rt) = super::runtime() {
            rt.record_untyped(&untyped(event));
        }
    }
}
