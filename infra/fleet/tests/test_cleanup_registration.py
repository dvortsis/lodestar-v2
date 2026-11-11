"""Registration proves both attachment directions; all API responses are fakes."""
import copy
import unittest
from datetime import timedelta
from unittest import mock

import cleanup_registration as reg
from linode import Linode, LinodeError
from tests.helpers import T0
from tests.test_live import ADMIN, FakeLinodeAPI


class Registration(unittest.TestCase):
    def setUp(self):
        self.api = FakeLinodeAPI()
        self.linode = Linode(self.api)
        self.fleet = {"created_at": T0.isoformat(), "expires_at": (T0 + timedelta(hours=44)).isoformat(),
                      "roles": {"main": {}, "worker": {}}}
        self.hosts = {
            "main": {"linode_id": "11", "firewall_id": "21", "address": "203.0.113.11", "admin_ipv4": [ADMIN]},
            "worker-01": {"linode_id": "12", "firewall_id": "22", "address": "203.0.113.12", "admin_ipv4": [ADMIN]},
        }

    def authorize(self):
        return reg.authorization(self.fleet, self.hosts, self.linode, "EUUID-FLEET", T0)

    def share(self):
        self.hosts["worker-01"]["firewall_id"] = "21"
        self.api.devices["21"].append({"entity": {"type": "linode", "id": 12}})
        self.api.devices["22"] = []

    def test_single_vm_requires_both_attachment_directions(self):
        self.fleet["roles"].pop("worker")
        doc = self.authorize()
        self.assertEqual([n["id"] for n in doc["linodes"]], [11])
        self.assertEqual([(w["id"], w["linode_ids"]) for w in doc["firewalls"]], [(21, [11])])
        self.assertIn(("GET", "/networking/firewalls/21/devices"), self.api.calls)
        self.assertIn(("GET", "/linode/instances/11/firewalls"), self.api.calls)

    def test_two_vms_with_distinct_firewalls(self):
        doc = self.authorize()
        self.assertEqual({w["id"]: w["linode_ids"] for w in doc["firewalls"]}, {21: [11], 22: [12]})
        for ident in (11, 12):
            self.assertIn(("GET", f"/linode/instances/{ident}/firewalls"), self.api.calls)

    def test_two_vms_can_share_one_exact_firewall(self):
        self.share()
        doc = self.authorize()
        self.assertEqual([(w["id"], w["linode_ids"]) for w in doc["firewalls"]], [(21, [11, 12])])
        for ident in (11, 12):
            self.assertIn(("GET", f"/linode/instances/{ident}/firewalls"), self.api.calls)

    def test_reported_empty_device_reproduction_is_refused(self):
        self.fleet["roles"].pop("worker")
        self.api.devices["21"] = []
        with self.assertRaises((reg.contract.Refused, LinodeError)):
            self.authorize()

    def test_shared_firewall_must_include_every_expected_member(self):
        self.share()
        self.api.devices["21"].pop()
        with self.assertRaises((reg.contract.Refused, LinodeError)):
            self.authorize()

    def test_forward_members_are_exact_unique_typed_ids(self):
        cases = ([], None, {}, [None], [{}], [{"entity": None}],
                 [{"entity": {"id": 11}}], [{"entity": {"type": "linode"}}],
                 [{"entity": {"type": "nodebalancer", "id": 11}}],
                 [{"entity": {"type": "linode", "id": 99}}],
                 [{"entity": {"type": "linode", "id": "11"}}],
                 [{"entity": {"type": "linode", "id": True}}],
                 [{"entity": {"type": "linode", "id": -11}}],
                 [{"entity": {"type": "linode", "id": 11}}] * 2)
        for devices in cases:
            with self.subTest(devices=devices), mock.patch.object(self.linode, "firewall_devices", return_value=devices):
                with self.assertRaises(reg.contract.Refused):
                    self.authorize()

    def test_shared_firewall_duplicate_cannot_substitute_for_missing_member(self):
        self.share()
        self.api.devices["21"][1] = copy.deepcopy(self.api.devices["21"][0])
        with self.assertRaises((reg.contract.Refused, LinodeError)):
            self.authorize()

    def test_reverse_bindings_are_exact_and_independent(self):
        cases = ([], ["22"], ["21", "22"], ["21", "21"], [21], [True], [None], None, {}, ["bad"])
        for reverse in cases:
            with self.subTest(reverse=reverse), mock.patch.object(self.linode, "linode_firewalls", return_value=reverse):
                with self.assertRaises(reg.contract.Refused):
                    self.authorize()

    def test_disagreeing_api_views_are_refused(self):
        def transport(method, path, body):
            if path.startswith("/linode/instances/11/firewalls?"):
                return {"data": [{"id": 22}], "page": 1, "pages": 1, "results": 1}
            return self.api(method, path, body)
        self.linode = Linode(transport)
        with self.assertRaises(reg.contract.Refused):
            self.authorize()

    def test_any_attachment_read_failure_is_refused(self):
        for method in ("firewall_devices", "linode_firewalls"):
            with self.subTest(method=method), mock.patch.object(self.linode, method, side_effect=LinodeError("read failed")):
                with self.assertRaises(LinodeError):
                    self.authorize()

    def test_existing_ownership_and_creation_boundaries_remain_enforced(self):
        mutations = (
            lambda: self.api.linodes["11"].update(region="eu-west"),
            lambda: self.api.linodes["11"].update(tags=[]),
            lambda: self.api.linodes["11"].update(created=(T0 - timedelta(seconds=1)).isoformat()),
            lambda: self.api.linodes["11"].update(label="foreign"),
            lambda: self.api.firewalls["21"].update(tags=[]),
            lambda: self.api.firewalls["21"].update(label="foreign"),
            lambda: self.api.firewalls["21"].update(created=(T0 - timedelta(seconds=1)).isoformat()),
        )
        for index, mutate in enumerate(mutations):
            with self.subTest(boundary=index):
                self.setUp()
                mutate()
                with self.assertRaises((reg.contract.Refused, LinodeError, KeyError)):
                    self.authorize()
        with mock.patch.object(self.linode, "check_account", side_effect=LinodeError("wrong account")):
            with self.assertRaises(LinodeError):
                self.authorize()


class CompletePagination(unittest.TestCase):
    def setUp(self):
        self.api = FakeLinodeAPI()

    def read(self, collection, mutate):
        def transport(method, path, body):
            result = self.api(method, path, body)
            if path.startswith(collection + "?"):
                mutate(result)
            return result
        ln = Linode(transport)
        if "/devices" in collection:
            return ln.firewall_devices(21)
        return ln.linode_firewalls(11)

    def test_incomplete_or_malformed_pages_cannot_satisfy_either_direction(self):
        mutations = {
            "missing pages": lambda r: r.pop("pages"),
            "missing page": lambda r: r.pop("page"),
            "missing count": lambda r: r.pop("results"),
            "string pages": lambda r: r.update(pages="1"),
            "boolean page": lambda r: r.update(page=True),
            "negative pages": lambda r: r.update(pages=-1),
            "boolean count": lambda r: r.update(results=True),
            "negative count": lambda r: r.update(results=-1),
            "wrong page": lambda r: r.update(page=2),
            "unbounded pages": lambda r: r.update(pages=51, results=5001),
            "truncated data": lambda r: r.update(results=2),
            "wrong page count": lambda r: r.update(pages=2),
            "missing data": lambda r: r.pop("data"),
            "non-list data": lambda r: r.update(data={}),
            "non-object entry": lambda r: r.update(data=[None]),
            "missing ID": lambda r: r.update(data=[{}]),
            "boolean ID": lambda r: r.update(data=[{"id": True}]),
        }
        for collection in ("/networking/firewalls/21/devices", "/linode/instances/11/firewalls"):
            for name, mutate in mutations.items():
                with self.subTest(collection=collection, case=name), self.assertRaises(LinodeError):
                    self.read(collection, mutate)

    def test_reads_complete_multi_page_inventory(self):
        calls = []
        def transport(method, path, body):
            page = 2 if "?page=2&" in path else 1
            calls.append(page)
            ids = range(1, 101) if page == 1 else [101]
            return {"data": [{"id": ident, "tags": ["lodestar"]} for ident in ids],
                    "page": page, "pages": 2, "results": 101}
        self.assertEqual(len(Linode(transport).linodes()), 101)
        self.assertEqual(calls, [1, 2])

    def test_page_count_that_disagrees_with_total_is_not_complete(self):
        # pages=1 while results=150 would otherwise end after 100 of 150 items.
        calls = []
        def transport(method, path, body):
            calls.append(path)
            return {"data": [{"id": n, "tags": ["lodestar"]} for n in range(1, 101)],
                    "page": 1, "pages": 1, "results": 150}
        with self.assertRaises(LinodeError):
            Linode(transport).linodes()
        self.assertEqual(len(calls), 1)

    def test_failed_repeated_changed_or_duplicate_second_page_is_not_partial_success(self):
        for failure in ("error", "repeat", "changed", "consistent change", "duplicate", "empty"):
            calls = []
            def transport(method, path, body):
                page = 2 if "?page=2&" in path else 1
                calls.append(page)
                if page == 2 and failure == "error":
                    raise LinodeError("second page failed")
                data = [{"id": n, "tags": ["lodestar"]} for n in (range(1, 101) if page == 1 else [101])]
                result = {"data": data, "page": page, "pages": 2, "results": 101}
                if page == 2:
                    if failure == "repeat":
                        result["page"] = 1
                    elif failure == "changed":
                        result["results"] = 102
                    elif failure == "consistent change":
                        # A self-consistent but different total on page 2 (150 with 50 items).
                        result["results"] = 150
                        result["data"] = [{"id": n, "tags": ["lodestar"]} for n in range(101, 151)]
                    elif failure == "duplicate":
                        result["data"][0]["id"] = 1
                    elif failure == "empty":
                        result["data"] = []
                return result
            with self.subTest(failure=failure), self.assertRaises(LinodeError):
                Linode(transport).linodes()
            self.assertEqual(calls, [1, 2])


if __name__ == "__main__":
    unittest.main()
