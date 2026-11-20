// Append inside the reviewed scheduler module, after its supplied tests.
// Both tests are expected to fail against the supplied proposal after the
// mechanical borrow-scope repairs described in verify-proposal.ps1.
#[cfg(test)]
mod review_regressions {
    use super::*;
    use tempfile::NamedTempFile;

    struct ReviewClock {
        wall: AtomicU64,
        mono: AtomicU64,
    }

    impl Clock for ReviewClock {
        fn wall_unix_secs(&self) -> Result<u32, SchedulerError> {
            Ok(self.wall.load(Ordering::SeqCst) as u32)
        }

        fn monotonic_elapsed_secs(&self) -> u64 {
            self.mono.load(Ordering::SeqCst)
        }
    }

    fn clock() -> Arc<ReviewClock> {
        Arc::new(ReviewClock {
            wall: AtomicU64::new(1_700_000_000),
            mono: AtomicU64::new(100),
        })
    }

    fn endpoint(n: u8) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::new(n, n, n, n), 6881)
    }

    #[tokio::test]
    async fn forward_jump_then_wal_commit_then_restart_must_preserve_a() {
        let db = NamedTempFile::new().unwrap();
        let clock = clock();
        let cfg = SchedulerConfig::default();
        let sched =
            SamplerScheduler::open_with_config(db.path(), cfg.clone(), clock.clone()).unwrap();
        let a = endpoint(8);
        let b = endpoint(9);
        assert!(sched.consider_candidate(a).unwrap());
        let p = sched.acquire_next_dispatch().await.unwrap().unwrap();
        sched
            .complete_dispatch(
                p,
                SampleOutcome::Supported {
                    advertised_interval_sec: Some(21600),
                    novel_hashes_count: 0,
                },
            )
            .await
            .unwrap();
        clock.mono.store(110, Ordering::SeqCst);
        clock.wall.store(1_700_086_410, Ordering::SeqCst);
        assert!(sched.consider_candidate(b).unwrap());
        let p = sched.acquire_next_dispatch().await.unwrap().unwrap();
        sched.complete_dispatch(p, SampleOutcome::Timeout).await.unwrap();
        drop(sched);
        clock.wall.store(1_700_086_411, Ordering::SeqCst);
        let restarted = SamplerScheduler::open_with_config(db.path(), cfg, clock).unwrap();
        assert!(
            !restarted.consider_candidate(a).unwrap(),
            "A lost its six-hour cooldown after only ten monotonic seconds"
        );
    }

    #[tokio::test]
    async fn second_scheduler_must_not_bypass_active_cooldown() {
        let db = NamedTempFile::new().unwrap();
        let cfg = SchedulerConfig::default();
        let a = SamplerScheduler::open_with_config(db.path(), cfg.clone(), clock()).unwrap();
        let b = SamplerScheduler::open_with_config(db.path(), cfg, clock()).unwrap();
        let x = endpoint(8);
        let y = endpoint(9);
        assert!(a.consider_candidate(x).unwrap());
        let p = a.acquire_next_dispatch().await.unwrap().unwrap();
        a.complete_dispatch(
            p,
            SampleOutcome::Supported {
                advertised_interval_sec: Some(21600),
                novel_hashes_count: 0,
            },
        )
        .await
        .unwrap();
        assert!(b.consider_candidate(y).unwrap());
        let p = b.acquire_next_dispatch().await.unwrap().unwrap();
        b.complete_dispatch(p, SampleOutcome::Timeout).await.unwrap();
        assert!(b.consider_candidate(x).unwrap());
        assert!(
            b.acquire_next_dispatch().await.unwrap().is_none(),
            "second scheduler dispatched X during first scheduler's six-hour cooldown"
        );
    }
}
