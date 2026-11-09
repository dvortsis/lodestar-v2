import json
import unittest

from lock import ACQUIRED, BUSY, LOCK_KEY, REAPING, FleetLock, LockLost, MemoryStore, S3Store
from tests.helpers import Clock


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
