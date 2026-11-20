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

    #[tokio::test]
    async fn expired_hot_nodes_must_be_reclaimable_when_ledger_is_full() {
        let db = NamedTempFile::new().unwrap();
        let c = clock();
        let cfg = SchedulerConfig {
            max_cooldown_entries: 1,
            ..SchedulerConfig::default()
        };
        let s = SamplerScheduler::open_with_config(db.path(), cfg, c.clone()).unwrap();
        let hot_ep = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 1), 6881);
        let new_ep = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 2), 6881);
        assert!(s.consider_candidate(hot_ep).unwrap());
        let permit = s.acquire_next_dispatch().await.unwrap().unwrap();
        s.complete_dispatch(
            permit,
            SampleOutcome::Supported {
                advertised_interval_sec: Some(60),
                novel_hashes_count: 5,
            },
        )
        .await
        .unwrap();
        advance(&c, 120);
        assert!(
            s.consider_candidate(new_ep).unwrap(),
            "ledger remained permanently wedged by an expired hot_heap entry that reclaim_expired_locked never inspects"
        );
    }

    #[tokio::test]
    async fn cancelled_completion_on_hot_node_must_not_reinsert_unsupported_endpoint_into_hot_heap() {
        let db = NamedTempFile::new().unwrap();
        let c = clock();
        let s =
            SamplerScheduler::open_with_config(db.path(), SchedulerConfig::default(), c.clone())
                .unwrap();
        let hot_ep = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 11), 6881);
        let other_ep = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 12), 6881);
        assert!(s.consider_candidate(hot_ep).unwrap());
        let permit1 = s.acquire_next_dispatch().await.unwrap().unwrap();
        s.complete_dispatch(
            permit1,
            SampleOutcome::Supported {
                advertised_interval_sec: Some(60),
                novel_hashes_count: 5,
            },
        )
        .await
        .unwrap();
        advance(&c, 65);
        let permit2 = s.acquire_next_dispatch().await.unwrap().unwrap();
        assert_eq!(permit2.addr(), hot_ep);

        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_hot_completion BEFORE UPDATE ON bep51_cooldowns
             BEGIN SELECT RAISE(ABORT, 'completion failure'); END;",
        )
        .unwrap();
        assert!(
            s.complete_dispatch(permit2, SampleOutcome::ExplicitUnsupported)
                .await
                .is_err()
        );
        conn.execute_batch("DROP TRIGGER fail_hot_completion;").unwrap();

        advance(&c, 21_600);
        assert!(s.consider_candidate(other_ep).unwrap());
        let permit3 = s.acquire_next_dispatch().await.unwrap().unwrap();
        assert_ne!(
            permit3.addr(),
            hot_ep,
            "explicitly unsupported node was re-queued onto hot_heap with stale Supported novelty and prioritized ahead of ready candidates after failed completion commit"
        );
    }

    #[tokio::test]
    async fn failed_admission_wal_write_must_not_poison_endpoint_for_six_hours() {
        let db = NamedTempFile::new().unwrap();
        let c = clock();
        let s =
            SamplerScheduler::open_with_config(db.path(), SchedulerConfig::default(), c.clone())
                .unwrap();
        let target = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 21), 6881);
        assert!(s.consider_candidate(target).unwrap());

        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_admission BEFORE INSERT ON bep51_cooldowns
             BEGIN SELECT RAISE(ABORT, 'admission failure'); END;",
        )
        .unwrap();
        assert!(s.acquire_next_dispatch().await.is_err());
        conn.execute_batch("DROP TRIGGER fail_admission;").unwrap();

        advance(&c, 60);
        assert!(
            s.consider_candidate(target).unwrap(),
            "endpoint was poisoned in RAM for six hours after a failed WAL reservation even though no packet was dispatched"
        );
    }

    #[tokio::test]
    async fn group_commit_single_stale_sequence_must_not_abort_unrelated_peer_reservations() {
        let db = NamedTempFile::new().unwrap();
        let c = clock();
        let cfg = SchedulerConfig {
            wal_batch_max_wait: Duration::from_millis(250),
            ..SchedulerConfig::default()
        };
        let s = SamplerScheduler::open_with_config(db.path(), cfg, c).unwrap();
        let bad_ep = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 31), 6881);
        let good_ep = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 32), 6881);

        let conn = Connection::open(db.path()).unwrap();
        conn.execute(
            "INSERT INTO bep51_cooldowns (ip_u32, port, next_allowed_sec, support, last_novel, op_seq)
             VALUES (?1, ?2, 0, 0, 0, 999)",
            params![u32::from(*bad_ep.ip()), bad_ep.port()],
        )
        .unwrap();

        assert!(s.consider_candidate(bad_ep).unwrap());
        assert!(s.consider_candidate(good_ep).unwrap());

        let (r1, r2) = tokio::join!(s.acquire_next_dispatch(), s.acquire_next_dispatch());
        assert!(
            r1.is_ok() || r2.is_ok(),
            "single stale-sequence endpoint in group-commit batch rolled back unrelated peer reservation"
        );
    }

    #[tokio::test]
    async fn repeated_sub_threshold_restarts_must_not_accumulate_unbounded_forward_drift() {
        let db = NamedTempFile::new().unwrap();
        let c = clock();
        let cfg = SchedulerConfig::default();
        let ep = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 41), 6881);
        {
            let s =
                SamplerScheduler::open_with_config(db.path(), cfg.clone(), c.clone()).unwrap();
            assert!(s.consider_candidate(ep).unwrap());
            let permit = s.acquire_next_dispatch().await.unwrap().unwrap();
            s.complete_dispatch(
                permit,
                SampleOutcome::Supported {
                    advertised_interval_sec: Some(21_600),
                    novel_hashes_count: 0,
                },
            )
            .await
            .unwrap();
        }

        for _ in 0..36 {
            c.mono.fetch_add(1, Ordering::SeqCst);
            c.wall.fetch_add(600, Ordering::SeqCst);
            let reopened =
                SamplerScheduler::open_with_config(db.path(), cfg.clone(), c.clone()).unwrap();
            drop(reopened);
        }

        let final_sched = SamplerScheduler::open_with_config(db.path(), cfg, c).unwrap();
        assert!(
            !final_sched.consider_candidate(ep).unwrap(),
            "six-hour cooldown expired after only 36 monotonic seconds via repeated 600s restart skews"
        );
    }

    #[tokio::test]
    async fn iana_special_purpose_as112_and_amt_ipv4_endpoints_must_be_rejected() {
        let db = NamedTempFile::new().unwrap();
        let s =
            SamplerScheduler::open_with_config(db.path(), SchedulerConfig::default(), clock())
                .unwrap();
        for addr in [
            SocketAddrV4::new(Ipv4Addr::new(192, 31, 196, 1), 6881),
            SocketAddrV4::new(Ipv4Addr::new(192, 52, 193, 1), 6881),
            SocketAddrV4::new(Ipv4Addr::new(192, 175, 48, 1), 6881),
        ] {
            assert!(
                !s.consider_candidate(addr).unwrap(),
                "scheduler admitted IANA special-purpose AS112/AMT IPv4 endpoints rejected by lodestar_network_policy"
            );
        }
    }
}
