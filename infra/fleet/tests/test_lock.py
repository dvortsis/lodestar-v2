import json
import unittest
from datetime import timedelta

from lock import ACQUIRED, BUSY, LOCK_KEY, REAPING, FleetLock, LockLost, MemoryStore, S3Store
from tests.helpers import Clock
from timefmt import fmt


class Lock(unittest.TestCase):
    def setUp(self):
        self.clock, self.store = Clock(), MemoryStore()
        self.lock = FleetLock(self.store, 90, self.clock)

    def holder(self):
        return json.loads(self.store.objects[LOCK_KEY][0])

    def test_take_busy_release(self):
        self.assertEqual(self.lock.acquire("1-1"), ACQUIRED)
        self.assertEqual(self.holder()["expires_at"], "2026-11-08T21:30:00Z")
        self.assertEqual(self.lock.acquire("2-1"), BUSY)
        self.assertFalse(self.lock.release("2-1"))  # not the holder
        self.assertIn(LOCK_KEY, self.store.objects)
        self.assertTrue(self.lock.release("1-1"))
        self.assertEqual(self.lock.acquire("2-1"), ACQUIRED)

    def test_own_lost_put_is_ours(self):
        # The first put landed but its response was lost; the retry sees our own holder.
        self.assertEqual(self.lock.acquire("1-1"), ACQUIRED)
        self.assertEqual(self.lock.acquire("1-1"), ACQUIRED)
        self.assertTrue(self.lock.holds("1-1"))
        self.assertEqual(self.lock.acquire("1-2"), BUSY)  # another attempt of the run isn't us
        self.lock.preempt("1-1")
        self.assertEqual(self.lock.acquire("1-1"), REAPING)  # a reaping marker never counts as ours

    def test_a_lost_answer_on_a_conditional_put_is_ours(self):
        class LostAnswer(MemoryStore):  # the write lands, the answer is lost
            def put_if_match(self, key, body, etag):
                super().put_if_match(key, body, etag)
                return False

            def put_if_absent(self, key, body):
                if self.calls.count("put_if_absent") == 0:
                    self.calls.append("put_if_absent")
                    return False  # the first try sees the old holder
                super().put_if_absent(key, body)
                return False
        store = LostAnswer()
        lock = FleetLock(store, 90, self.clock)
        # The takeover path: an expired holder replaced by put_if_match.
        store.objects[LOCK_KEY] = (lock._body("run", "1-1"), '"0"')
        self.clock.sleep(91 * 60)
        self.assertEqual(lock.acquire("2-1"), ACQUIRED)
        self.assertEqual(json.loads(store.objects[LOCK_KEY][0])["run_id"], "2-1")
        # The released-between-calls path: the second put_if_absent lands silently.
        store2 = LostAnswer()
        lock2 = FleetLock(store2, 90, self.clock)
        self.assertEqual(lock2.acquire("3-1"), ACQUIRED)
        self.assertTrue(lock2.holds("3-1"))

    def test_a_lost_answer_is_ours_only_for_a_live_run_holder(self):
        class Replaced(MemoryStore):  # put_if_match answers no; the key now holds `after`
            after = None

            def put_if_match(self, key, body, etag):
                self.objects[key] = (self.after, '"x"')
                return False
        for name, kind, live in (("a live reaping marker with our ID", "reaping", True),
                                 ("our run ID, already expired", "run", False)):
            with self.subTest(name):
                store = Replaced()
                lk = FleetLock(store, 90, self.clock)
                store.objects[LOCK_KEY] = (lk._body("run", "1-1"), '"0"')
                self.clock.sleep(91 * 60)  # the old holder expires; takeover by put_if_match
                # Built now, so only the condition under test can reject it.
                ends = self.clock() + timedelta(minutes=90 if live else -1)
                store.after = json.dumps({"kind": kind, "run_id": "2-1", "expires_at": fmt(ends)}).encode()
                self.assertEqual(lk.acquire("2-1"), BUSY)
                # The control: the same takeover, and the re-read finds a live run holder of ours.
                store.objects[LOCK_KEY] = (json.dumps({"kind": "run", "run_id": "1-1", "expires_at": fmt(
                    self.clock() - timedelta(minutes=1))}).encode(), '"0"')
                store.after = json.dumps({"kind": "run", "run_id": "2-1",
                                          "expires_at": fmt(self.clock() + timedelta(minutes=90))}).encode()
                self.assertEqual(lk.acquire("2-1"), ACQUIRED)

    def test_expires_at_only_while_held(self):
        self.lock.acquire("1-1")
        self.assertEqual(self.lock.expires_at("1-1").isoformat(), "2026-11-08T21:30:00+00:00")
        with self.assertRaises(LockLost):
            self.lock.expires_at("2-1")
        self.clock.sleep(90 * 60)
        with self.assertRaises(LockLost):  # its own lease has run out
            self.lock.expires_at("1-1")

    def test_an_expired_own_holder_is_renewed_through_put_if_match(self):
        self.lock.acquire("1-1")
        first = self.holder()["expires_at"]
        self.clock.sleep(91 * 60)
        calls = len(self.store.calls)
        self.assertEqual(self.lock.acquire("1-1"), ACQUIRED)
        self.assertIn("put_if_match", self.store.calls[calls:])  # a fresh lease, not the stale one
        self.assertNotEqual(self.holder()["expires_at"], first)
        self.assertTrue(self.lock.holds("1-1"))

    def test_expired_lock_is_taken_over_conditionally(self):
        self.lock.acquire("1-1")
        self.clock.sleep(89 * 60)
        self.assertEqual(self.lock.acquire("2-1"), BUSY)
        self.clock.sleep(61)
        self.assertEqual(self.lock.acquire("2-1"), ACQUIRED)
        self.assertEqual(self.holder()["run_id"], "2-1")
        self.assertFalse(self.lock.release("1-1"))  # the old holder can't delete it

    def test_takeover_race_loses_cleanly(self):
        self.lock.acquire("1-1")
        self.clock.sleep(91 * 60)
        orig = self.store.get

        def racing_get(key):
            r = orig(key)
            self.store.put(key, b'{"kind":"run","run_id":"3-1","expires_at":"2026-11-08T23:00:00Z"}')
            return r
        self.store.get = racing_get
        self.assertEqual(self.lock.acquire("2-1"), BUSY)

    def test_release_race_does_not_delete_a_new_holder(self):
        self.lock.acquire("1-1")
        orig = self.store.get

        def racing_get(key):
            r = orig(key)
            self.store.put(key, b'{"kind":"run","run_id":"3-1","expires_at":"2026-11-08T23:00:00Z"}')
            return r
        self.store.get = racing_get
        self.assertFalse(self.lock.release("1-1"))
        self.store.get = orig
        self.assertEqual(self.holder()["run_id"], "3-1")

    def test_reaping_marker_preempts_and_fences(self):
        self.lock.acquire("1-1")
        self.lock.preempt("9-1")
        self.assertEqual(self.holder()["kind"], "reaping")
        self.assertEqual(self.lock.acquire("2-1"), REAPING)
        writes = []
        with self.assertRaises(LockLost):
            self.lock.fenced("1-1", lambda: writes.append("rules"))
        self.assertEqual(writes, [])
        self.lock.fenced("9-1", lambda: writes.append("rules"))
        self.assertEqual(writes, ["rules"])
        self.assertTrue(self.lock.release("9-1"))

    def test_fence_refuses_an_expired_lease(self):
        self.lock.acquire("1-1")
        self.clock.sleep(90 * 60)
        with self.assertRaises(LockLost):
            self.lock.fence("1-1")

    def test_unreadable_lock_fails_closed(self):
        self.store.put(LOCK_KEY, b"not json")
        self.assertEqual(self.lock.acquire("1-1"), BUSY)
        self.assertFalse(self.lock.holds("1-1"))
        self.lock.preempt("9-1")
        self.assertTrue(self.lock.holds("9-1"))

    def test_s3_store_refuses_dry_run(self):
        with self.assertRaises(RuntimeError):
            S3Store("bucket", live=False)


if __name__ == "__main__":
    unittest.main()
