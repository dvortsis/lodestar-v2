//! What every crate logs through (architecture §12.1): a static catalogue
//! of events, each with its own switch and counter, and typed wrappers for
//! every kind of data a line can carry.
//!
//! - **Declare** a crate's events once, in its `log_events.rs`, with
//!   [`catalogue!`]: a stable id, a component, a level, the message, and
//!   the fields with the kind each carries. The binary gathers every
//!   crate's [`Catalogue`] in an explicit list and [`register`]s it.
//! - **Write** with [`log_event!`]`(EVENT, field = wrapper, …)`. It takes
//!   no format string: the message is the catalogue's, and every dynamic
//!   part is a declared, typed field. A field the event didn't declare, or
//!   a value of the wrong kind, doesn't compile.
//! - **Cost:** an event that's off costs one relaxed load and one relaxed
//!   increment (the counter counts either way, for the volume estimates).
//!   Field values are evaluated only when it's on.
//! - **Rendering** is the outputs' (the binary's [`Sink`]): each shows a
//!   kind by its own switch. Off, a torrent is `ref:xxxxxxxx` and anything
//!   else `[hidden]`. URLs lose their credentials at every setting, and an
//!   error shows its message only when its type is [`PlainError`].
//! - **Never loggable:** passwords, API keys, feed tokens, cookies, secrets
//!   and sealed values have no wrapper, and the names of quarantined,
//!   dropped or hash-listed items must never be passed to [`Name`] or
//!   [`Path`].
//!
//! It depends on `tracing` only, for the fallback without a sink.

mod dispatch;
mod event;
pub mod url;
mod value;

pub use dispatch::{
    KindStatus, Output, OutputStatus, Record, Sink, Status, average_line, catalogues, check_ids,
    emit, events, find, note_written, rate_per_hour, register, set_sink, set_status, status,
};
pub use event::{Catalogue, Component, Event, FieldKind, Kind, Level, Mask};
pub use value::{
    Client, Count, Detail, Error, HIDDEN, IntoValue, Listen, Location, Name, Path, Peer,
    PlainError, Query, Reference, Rendered, SetupToken, Torrent, Url, Value, kind, pairs_torrent,
};

/// Declares a crate's events and its `CATALOGUE`.
///
/// ```
/// lodestar_log::catalogue! {
///     "my-crate";
///     /// A fetch failed.
///     FETCH_FAILED {
///         id: "fetch.failed",
///         component: Fetch,
///         level: Warn,
///         message: "Metadata fetch failed",
///         fields: { torrent: Hash, error: Error },
///     }
///     /// The process started: required, so it can't be turned off.
///     STARTED {
///         id: "app.start",
///         component: System,
///         level: Info,
///         required: true,
///         message: "Lodestar started",
///         fields: {},
///     }
/// }
/// # fn main() {
/// let e = lodestar_log::Error::kind("timeout");
/// lodestar_log::log_event!(FETCH_FAILED, torrent = lodestar_log::Torrent(&[0; 20]), error = e);
/// assert_eq!(FETCH_FAILED::EVENT.count(), 1);
/// # }
/// ```
///
/// Optional keys, in this order after `level`: `default_on: false` (a
/// high-volume event that joins at Detailed), `required: true` and
/// `opt_in: FilterChange`. Field kinds are the [`kind`] markers.
#[macro_export]
macro_rules! catalogue {
    (
        $crate_name:literal;
        $(
            $(#[$meta:meta])*
            $name:ident {
                id: $id:literal,
                component: $component:ident,
                level: $level:ident,
                $(default_on: $default_on:literal,)?
                $(required: $required:literal,)?
                $(opt_in: $opt_in:ident,)?
                message: $message:literal,
                fields: { $($field:ident: $fkind:ident),* $(,)? } $(,)?
            }
        )*
    ) => {
        $(
            $(#[$meta])*
            #[allow(non_snake_case, dead_code)]
            pub mod $name {
                /// The event: its switch, its counter and its declaration.
                pub static EVENT: $crate::Event = $crate::Event::declare(
                    $id,
                    $crate::Component::$component,
                    $crate::Level::$level,
                    $message,
                    { let _on = true; $(let _on = $default_on;)? _on },
                    { let _required = false; $(let _required = $required;)? _required },
                    { let _opt: Option<$crate::Kind> = None; $(let _opt = Some($crate::Kind::$opt_in);)? _opt },
                    &[$((stringify!($field), <$crate::kind::$fkind as $crate::kind::Marker>::KIND)),*],
                );

                /// One function per declared field: a field the event didn't
                /// declare, or a value of another kind, doesn't compile.
                pub mod fields {
                    $(
                        #[inline]
                        pub fn $field<'a>(
                            value: impl $crate::IntoValue<'a, $crate::kind::$fkind>,
                        ) -> $crate::Value<'a> {
                            value.into_value()
                        }
                    )*
                }
            }
        )*

        /// This crate's events, for the binary's list.
        pub static CATALOGUE: $crate::Catalogue = $crate::Catalogue {
            name: $crate_name,
            events: &[$(&$name::EVENT),*],
        };
    };
}

/// Writes a catalogued event: `log_event!(EVENT, field = wrapper, …)`.
///
/// It checks the event's switch, bumps its counter (on or off), and only
/// then evaluates the fields and hands the record to the outputs.
///
/// A field the event didn't declare doesn't compile:
///
/// ```compile_fail
/// lodestar_log::catalogue! { "t"; E { id: "t.e", component: System, level: Info, message: "e", fields: { count: Size } } }
/// fn main() { lodestar_log::log_event!(E, name = lodestar_log::Name("n")); }
/// ```
///
/// Nor does a value of another kind (a name in a size field):
///
/// ```compile_fail
/// lodestar_log::catalogue! { "t"; E { id: "t.e", component: System, level: Info, message: "e", fields: { count: Size } } }
/// fn main() { lodestar_log::log_event!(E, count = lodestar_log::Name("n")); }
/// ```
///
/// Nor a label made at runtime (a label is the code's own `&'static str`):
///
/// ```compile_fail
/// lodestar_log::catalogue! { "t"; E { id: "t.e", component: System, level: Info, message: "e", fields: { what: Label } } }
/// fn main() { let s = String::from("x"); lodestar_log::log_event!(E, what = s.as_str()); }
/// ```
///
/// The same event, written right:
///
/// ```
/// lodestar_log::catalogue! { "t"; E { id: "t.e", component: System, level: Info, message: "e", fields: { count: Size, what: Label } } }
/// fn main() { lodestar_log::log_event!(E, count = 3u64, what = "x"); }
/// ```
#[macro_export]
macro_rules! log_event {
    ($($event:ident)::+ $(, $field:ident = $value:expr)* $(,)?) => {{
        use $($event)::+ as __lodestar_event;
        let event: &'static $crate::Event = &__lodestar_event::EVENT;
        event.hit();
        if event.is_on() {
            $crate::emit(
                event,
                &[$((stringify!($field), __lodestar_event::fields::$field($value))),*],
            );
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    catalogue! {
        "lodestar-log-tests";
        /// A test event.
        PLAIN {
            id: "test.plain",
            component: System,
            level: Info,
            message: "plain",
            fields: { torrent: Hash, size: Size, client: Label },
        }
        QUIET {
            id: "test.quiet",
            component: Fetch,
            level: Info,
            default_on: false,
            message: "quiet",
            fields: {},
        }
        NEEDED {
            id: "test.needed",
            component: System,
            level: Debug,
            required: true,
            message: "needed",
            fields: {},
        }
        DETAIL {
            id: "test.detail",
            component: Filters,
            level: Info,
            opt_in: FilterChange,
            message: "detail",
            fields: { change: FilterChange },
        }
        PEERS {
            id: "test.peers",
            component: Fetch,
            level: Debug,
            message: "peers",
            fields: { torrent: Hash, peer: Peer },
        }
    }

    #[test]
    fn declarations_start_as_normal_has_them() {
        assert!(PLAIN::EVENT.is_on());
        assert!(!QUIET::EVENT.is_on(), "off by default");
        assert!(NEEDED::EVENT.is_on(), "required, even at debug");
        assert!(!DETAIL::EVENT.is_on(), "opt-ins start off");
        assert!(!PEERS::EVENT.is_on(), "debug");
        assert_eq!(
            PLAIN::EVENT.fields,
            &[
                ("torrent", FieldKind::Data(Kind::Hash)),
                ("size", FieldKind::Data(Kind::Size)),
                ("client", FieldKind::Label),
            ]
        );
        assert_eq!(CATALOGUE.events.len(), 5);
        assert_eq!(PLAIN::EVENT.label_key(), "logs.event.test.plain.label");
        assert_eq!(
            PEERS::EVENT.kinds(),
            [Kind::Hash, Kind::Peer, Kind::PeerActivity]
        );
        assert_eq!(DETAIL::EVENT.kinds(), [Kind::FilterChange]);
        check_ids(&[&CATALOGUE]).unwrap();
    }

    #[test]
    fn required_events_stay_on() {
        NEEDED::EVENT.set_on(false);
        assert!(NEEDED::EVENT.is_on());
    }

    #[test]
    fn an_event_thats_off_only_counts() {
        let evaluated = std::cell::Cell::new(0);
        let size = || {
            evaluated.set(evaluated.get() + 1);
            5u64
        };
        let before = QUIET::EVENT.count();
        log_event!(QUIET);
        assert_eq!(QUIET::EVENT.count(), before + 1);
        // Its own event: the module's are shared statics, and another
        // test reading `PLAIN`'s switch mustn't see this one toggle it.
        mod toggled {
            crate::catalogue! {
                "toggled";
                TOGGLED {
                    id: "test.toggled",
                    component: System,
                    level: Info,
                    message: "toggled",
                    fields: { torrent: Hash, size: Size, client: Label },
                }
            }
        }
        use toggled::TOGGLED;
        let before = TOGGLED::EVENT.count();
        TOGGLED::EVENT.set_on(false);
        log_event!(TOGGLED, size = size(), client = "x");
        assert_eq!(evaluated.get(), 0, "fields aren't evaluated while it's off");
        assert_eq!(TOGGLED::EVENT.count(), before + 1, "but it counts");
        TOGGLED::EVENT.set_on(true);
        log_event!(
            TOGGLED,
            torrent = Torrent(&[1; 20]),
            size = size(),
            client = "x"
        );
        assert_eq!(evaluated.get(), 1);
        assert_eq!(TOGGLED::EVENT.count(), before + 2);
    }

    #[test]
    fn verbosity_turns_on_by_level_and_default() {
        assert!(PLAIN::EVENT.on_under(Level::Info));
        assert!(!PLAIN::EVENT.on_under(Level::Warn));
        assert!(!QUIET::EVENT.on_under(Level::Info));
        assert!(QUIET::EVENT.on_under(Level::Debug), "joins at Detailed");
        assert!(
            !DETAIL::EVENT.on_under(Level::Trace),
            "no preset turns an opt-in on"
        );
    }

    #[test]
    fn ids_are_checked() {
        catalogue! {
            "dupes";
            A { id: "x.one", component: System, level: Info, message: "a", fields: {} }
            B { id: "x.one", component: System, level: Info, message: "b", fields: {} }
        }
        assert!(check_ids(&[&CATALOGUE]).unwrap_err().contains("twice"));
        mod bad {
            crate::catalogue! {
                "bad";
                C { id: "NoDot", component: System, level: Info, message: "c", fields: {} }
            }
        }
        assert!(check_ids(&[&bad::CATALOGUE]).is_err());
    }

    #[test]
    fn the_text_line_hides_by_mask() {
        let h = [0xcd; 20];
        let fields = [
            ("torrent", Value::Torrent(&h)),
            ("peer", Value::Peer("203.0.113.7:1".parse().unwrap())),
        ];
        let record = Record {
            event: &PEERS::EVENT,
            fields: &fields,
        };
        assert_eq!(
            record.text(Mask::PRIVATE, None),
            "peers torrent=[hidden] peer=[hidden]"
        );
        let all = Mask(u32::MAX);
        assert_eq!(
            record.text(all, None),
            format!("peers torrent={} peer=203.0.113.7:1", "cd".repeat(20))
        );
    }
}
