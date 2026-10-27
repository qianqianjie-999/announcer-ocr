import base64
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

import config_web as cw


def basic(user, password):
    raw = f"{user}:{password}".encode("utf-8")
    return "Basic " + base64.b64encode(raw).decode("ascii")


class TestWebAuth(unittest.TestCase):
    def test_loopback_without_password_keeps_local_development_open(self):
        self.assertEqual(cw.ensure_web_password("127.0.0.1", ""), "")
        self.assertEqual(cw.ensure_web_password("::1", ""), "")

    def test_non_loopback_without_password_generates_secret(self):
        password = cw.ensure_web_password("0.0.0.0", "")
        self.assertGreaterEqual(len(password), 20)
        self.assertNotEqual(password, "0.0.0.0")

    def test_configured_password_is_preserved(self):
        self.assertEqual(cw.ensure_web_password("0.0.0.0", "fixed-secret"), "fixed-secret")

    def test_basic_auth_accepts_exact_credentials(self):
        self.assertTrue(cw.basic_auth_ok(basic("admin", "fixed-secret"), "fixed-secret"))

    def test_basic_auth_rejects_wrong_or_malformed_credentials(self):
        self.assertFalse(cw.basic_auth_ok(basic("admin", "wrong"), "fixed-secret"))
        self.assertFalse(cw.basic_auth_ok(basic("guest", "fixed-secret"), "fixed-secret"))
        self.assertFalse(cw.basic_auth_ok("Basic not-base64!", "fixed-secret"))
        self.assertFalse(cw.basic_auth_ok(None, "fixed-secret"))

    def test_empty_password_means_auth_disabled_for_local_tests(self):
        self.assertTrue(cw.basic_auth_ok(None, ""))


class TestWebAuthWiring(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.old_password = cw.WEB_PASSWORD
        cw.WEB_PASSWORD = "fixed-secret"
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), cw.Handler)
        cls.thread = threading.Thread(target=cls.srv.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = "http://127.0.0.1:%d" % cls.srv.server_port

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()
        cls.srv.server_close()
        cls.thread.join(timeout=2)
        cw.WEB_PASSWORD = cls.old_password

    def test_get_and_post_require_auth(self):
        for method, path, data in (
            ("GET", "/", None),
            ("POST", "/api/maint", b'{"action":"reboot"}'),
        ):
            req = urllib.request.Request(self.base + path, method=method, data=data)
            with self.assertRaises(urllib.error.HTTPError) as caught:
                urllib.request.urlopen(req, timeout=3)
            self.assertEqual(caught.exception.code, 401)

    def test_authorized_get_reaches_page(self):
        req = urllib.request.Request(
            self.base + "/",
            headers={"Authorization": basic("admin", "fixed-secret")},
        )
        with urllib.request.urlopen(req, timeout=3) as response:
            self.assertEqual(response.status, 200)
            self.assertIn("叫号播报", response.read().decode("utf-8"))


if __name__ == "__main__":
    unittest.main()
