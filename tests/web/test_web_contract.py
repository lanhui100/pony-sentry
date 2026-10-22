import os
import subprocess
import unittest

class TestWebContract(unittest.TestCase):
    def test_web_package_json_exists(self):
        self.assertTrue(os.path.exists("web/package.json"))

    def test_web_dist_built(self):
        self.assertTrue(os.path.exists("web/dist/index.html"))
        with open("web/dist/index.html", "r", encoding="utf-8") as f:
            content = f.read()
        self.assertIn("<!DOCTYPE html>", content)
        self.assertIn('id="app"', content)

    def test_web_build_command(self):
        res = subprocess.run(["pnpm", "run", "build"], cwd="web", capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Web build failed: {res.stderr}")
        self.assertIn("built in", res.stdout + res.stderr)
        self.assertTrue(os.path.exists("web/dist/index.html"))

    def test_load_error_diagnostics_are_wired(self):
        from pathlib import Path
        app = Path("web/src/App.vue").read_text(encoding="utf-8")
        self.assertIn("loadFailureDetail(loadFailures.value)", app)
        self.assertIn("data-testid=\"load-error-detail\"", app)
        self.assertIn("err.name = response.status === 401 || response.status === 403 ? 'AuthError' : 'HttpError'", app)

    def test_console_uses_server_session_not_embedded_static_token(self):
        from pathlib import Path
        app = Path("web/src/App.vue").read_text(encoding="utf-8")
        self.assertIn("data-testid=\"login-dialog\"", app)
        self.assertIn("data-testid=\"logout-button\"", app)
        self.assertIn("/api/v1/auth/session", app)
        self.assertIn("sessionHeaders(fetchOptions.method)", app)
        self.assertNotIn("VITE_PONY_SENTRY_TOKEN", app)
        self.assertNotIn("localStorage.getItem('pony_sentry_token')", app)
        self.assertNotIn("x-admin-token", app)

    def test_web_dist_is_rebuilt_without_baked_token(self):
        import re
        from pathlib import Path
        dist = Path("web/dist/index.html").read_text(encoding="utf-8")
        self.assertIn("鉴权失败：登录会话已失效", dist)
        self.assertNotRegex(dist, r"x-admin-token[^\n]{0,32}[0-9a-f]{64}")

if __name__ == '__main__':
    unittest.main()
