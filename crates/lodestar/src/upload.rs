//! The upload route's way into the pipeline (UP-0,
//! `docs/decisions/2026-11-12-UP-0.md`): `lodestar_api::upload`'s
//! [`Sink`] over the pipeline's `local_torrent` handle. The API crate
//! doesn't depend on the pipeline, so the host joins them here.

use std::sync::Arc;

use lodestar_api::ops::BoxFuture;
use lodestar_api::upload::{Outcome, Sink, Unavailable, Uploader};
use lodestar_pipeline::local_torrent::{self, Uploads};

/// The pipeline's uploads, as the route sees them.
#[derive(Debug)]
struct PipelineSink(Uploads);

impl Sink for PipelineSink {
    fn submit(&self, body: Vec<u8>) -> BoxFuture<'_, Result<Outcome, Unavailable>> {
        Box::pin(async move {
            self.0
                .submit(body)
                .await
                .map(outcome)
                .map_err(|local_torrent::Closed| Unavailable)
        })
    }
}

fn outcome(o: local_torrent::Outcome) -> Outcome {
    match o {
        local_torrent::Outcome::Accepted { key, private } => Outcome::Accepted {
            info_hash: key.0,
            private,
        },
        local_torrent::Outcome::Malformed => Outcome::Malformed,
        local_torrent::Outcome::Refused => Outcome::Refused,
        local_torrent::Outcome::Held => Outcome::Held,
    }
}

/// The route's [`Uploader`] for this process: one, since it holds the
/// rate buckets.
pub fn uploader(uploads: Uploads) -> Uploader {
    Uploader::new(Arc::new(PipelineSink(uploads)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_metainfo::InfoHash;

    #[test]
    fn every_outcome_maps_to_its_own() {
        let key = InfoHash([7; 20]);
        assert_eq!(
            outcome(local_torrent::Outcome::Accepted { key, private: true }),
            Outcome::Accepted {
                info_hash: [7; 20],
                private: true
            }
        );
        assert_eq!(
            outcome(local_torrent::Outcome::Malformed),
            Outcome::Malformed
        );
        assert_eq!(outcome(local_torrent::Outcome::Refused), Outcome::Refused);
        assert_eq!(outcome(local_torrent::Outcome::Held), Outcome::Held);
    }
}
