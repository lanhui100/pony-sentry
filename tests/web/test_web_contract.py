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
        res = subprocess.run(["pnpm", "--filter", "pony-sentry-web", "run", "build"], cwd="web", capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Web build failed: {res.stderr}")
        self.assertTrue(os.path.exists("web/dist/index.html"))

if __name__ == '__main__':
    unittest.main()
