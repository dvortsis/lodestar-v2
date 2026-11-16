// Append to the mechanically repaired scheduler module. These counterexamples
// use local SQLite and mock clocks; no network traffic is sent.
#[cfg(test)]
mod followup_review {
    use super::*;
    use tempfile::NamedTempFile;

    struct TestClock {
        wall: AtomicU64,
        mono: AtomicU64,
    }

    impl Clock for TestClock {
        fn wall_unix_secs(&self) -> Result<u32, SchedulerError> {
            Ok(self.wall.load(Ordering::SeqCst) as u32)
        }

        fn monotonic_elapsed_secs(&self) -> u64 {
            self.mono.load(Ordering::SeqCst)
        }
    }

    fn clock() -> Arc<TestClock> {
        Arc::new(TestClock {
            wall: AtomicU64::new(1_700_000_000),
            mono: AtomicU64::new(100),
        })
    }

    fn advance(c: &TestClock, seconds: u64) {
        c.wall.fetch_add(seconds, Ordering::SeqCst);
        c.mono.fetch_add(seconds, Ordering::SeqCst);
    }

    fn endpoint() -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 6881)
    }

    #[tokio::test]
    async fn failed_completion_must_preserve_received_interval() {
        let db = NamedTempFile::new().unwrap();
        let c = clock();
        let s =
            SamplerScheduler::open_with_config(db.path(), SchedulerConfig::default(), c.clone())
                .unwrap();
        assert!(s.consider_candidate(endpoint()).unwrap());
        let permit = s.acquire_next_dispatch().await.unwrap().unwrap();
        advance(&c, 10);
        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_completion BEFORE UPDATE ON bep51_cooldowns
             BEGIN SELECT RAISE(ABORT, 'completion failure'); END;",
        )
        .unwrap();
        assert!(
            s.complete_dispatch(
                permit,
                SampleOutcome::Supported {
                    advertised_interval_sec: Some(21600),
                    novel_hashes_count: 0,
                },
            )
            .await
            .is_err()
        );
        conn.execute_batch("DROP TRIGGER fail_completion;").unwrap();
        advance(&c, 21590);
        s.consider_candidate(endpoint()).unwrap();
        assert!(
            s.acquire_next_dispatch().await.unwrap().is_none(),
            "redispatched ten seconds before the received interval expired after a failed completion commit"
        );
    }

    #[tokio::test]
    async fn foreign_permit_must_be_rejected() {
        let db_a = NamedTempFile::new().unwrap();
        let db_b = NamedTempFile::new().unwrap();
        let a =
            SamplerScheduler::open_with_config(db_a.path(), SchedulerConfig::default(), clock())
                .unwrap();
        let b =
            SamplerScheduler::open_with_config(db_b.path(), SchedulerConfig::default(), clock())
                .unwrap();
        assert!(a.consider_candidate(endpoint()).unwrap());
        assert!(b.consider_candidate(endpoint()).unwrap());
        let permit_a = a.acquire_next_dispatch().await.unwrap().unwrap();
        let _permit_b = b.acquire_next_dispatch().await.unwrap().unwrap();
        let result = b.complete_dispatch(permit_a, SampleOutcome::Timeout).await;
        assert!(
            result.is_err(),
            "scheduler B accepted a permit owned by scheduler A with a different database"
        );
    }
}
