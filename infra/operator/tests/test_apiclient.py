import json
import threading
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer

from tests import helpers  # noqa: F401 - sys.path
import apiclient
from apiclient import ApiError


class Hosts(unittest.TestCase):
    def test_only_the_fixed_https_hosts(self):
        apiclient.check_url("https://api.anthropic.com/v1/sessions", "/v1/sessions")
        apiclient.check_url("https://api.github.com/repos/o/r", "/repos/o/r")
        for url, path in (("http://api.anthropic.com/v1/x", "/v1/x"), ("https://evil.example/v1/x", "/v1/x"),
                          ("https://api.anthropic.com.evil.example/v1/x", "/v1/x"),
                          ("https://api.anthropic.com//evil.example/x", "//evil.example/x"),
                          ("https://api.anthropic.com@evil.example/x", "@evil.example/x"),
                          ("https://api.github.com/repos/o/r", "/repos/o/r/other")):
            with self.subTest(url=url), self.assertRaises(ApiError):
                apiclient.check_url(url, path)

    def test_a_refused_host_makes_no_request(self):
        opened = []
        with self.assertRaises(ApiError):
            apiclient._send("https://evil.example/v1/x", "GET", "/v1/x", None, {}, open_=lambda *a, **k: opened.append(a))
        self.assertEqual(opened, [])


class Redirects(unittest.TestCase):
    def test_a_redirect_is_never_followed(self):
        hits = []

        class H(BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                hits.append(self.path)
                if self.path == "/start":
                    self.send_response(302)
                    self.send_header("Location", "/elsewhere")
                    self.end_headers()
                else:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(json.dumps({"followed": True}).encode())

            def log_message(self, *a):
                pass
        srv = HTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.shutdown)
        req = urllib.request.Request(f"http://127.0.0.1:{srv.server_port}/start", headers={"x-api-key": "k"})
        with self.assertRaises(urllib.error.HTTPError) as cm:
            apiclient.opener().open(req, timeout=10)
        self.assertEqual(cm.exception.code, 302)
        self.assertEqual(hits, ["/start"])

    def test_errors_carry_no_ids_or_bodies(self):
        def fail(req, timeout):
            raise urllib.error.HTTPError(req.full_url, 404, "nope", {}, None)
        with self.assertRaises(ApiError) as cm:
            apiclient._send("https://api.anthropic.com/v1/sessions/sesn_SECRET/archive?x=1", "POST",
                            "/v1/sessions/sesn_SECRET/archive?x=1", {"a": "TOKEN"}, {}, open_=fail)
        self.assertEqual(str(cm.exception), "POST /v1/sessions failed: HTTP 404")


class Pages(unittest.TestCase):
    def test_follows_next_page_and_refuses_past_the_limit(self):
        calls = []

        def api(m, path, b):
            calls.append(path)
            return {"data": [{"id": len(calls)}], "next_page": "p" if len(calls) < 3 else None}
        self.assertEqual([d["id"] for d in apiclient.pages(api, "/v1/sessions", {"include_archived": "true"})], [1, 2, 3])
        self.assertIn("include_archived=true", calls[0])
        self.assertIn("page=p", calls[1])
        endless = lambda m, p, b: {"data": [], "next_page": "again"}  # noqa: E731
        with self.assertRaises(ApiError):
            apiclient.pages(endless, "/v1/sessions", {})
        with self.assertRaises(ApiError):
            apiclient.pages(lambda m, p, b: {"next_page": None}, "/v1/sessions", {})
