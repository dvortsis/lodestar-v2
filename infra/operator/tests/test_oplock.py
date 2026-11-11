import json
import unittest
from datetime import timedelta

from tests.helpers import Clock
from lock import MemoryStore
from oplock import LOCK_KEY, OperatorLock


class Lock(unittest.TestCase):
    def setUp(self):
        self.store, self.clock = MemoryStore(), Clock()
        self.lock = OperatorLock(self.store, self.clock)

    def test_one_holder_then_release(self):
        self.assertTrue(self.lock.acquire("1-1"))
        self.assertFalse(self.lock.acquire("2-1"))
        self.assertFalse(self.lock.acquire("1-1"))  # never twice, even for the same run
        self.assertFalse(self.lock.release("2-1"))
        self.assertTrue(self.lock.release("1-1"))
        self.assertTrue(self.lock.acquire("2-1"))

    def test_expired_lock_is_taken_over_with_if_match(self):
        self.lock.acquire("1-1")
        self.clock.t += timedelta(minutes=61)
        self.assertTrue(self.lock.acquire("2-1"))
        self.assertEqual(json.loads(self.store.objects[LOCK_KEY][0])["run_id"], "2-1")
        self.assertIn("put_if_match", self.store.calls)

    def test_unreadable_lock_fails_closed(self):
        self.store.put(LOCK_KEY, b"junk")
        self.clock.t += timedelta(days=9)
        self.assertFalse(self.lock.acquire("1-1"))
        self.assertFalse(self.lock.release("1-1"))
