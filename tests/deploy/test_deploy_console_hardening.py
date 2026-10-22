#!/usr/bin/env python3
"""部署脚本的机械加固门禁。

这些断言不替代真实部署，但能阻止回退到“无备份、无回滚、无 EXIT 还原、
二进制 world-readable”的旧实现。
"""
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "deploy" / "scripts" / "deploy-console.sh"


class TestDeployConsoleHardening(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = SCRIPT.read_text(encoding="utf-8")

    def test_shell_syntax(self):
        result = subprocess.run(
            ["bash", "-n", str(SCRIPT)], capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_exit_trap_restores_worktree_dist(self):
        self.assertIn("trap cleanup EXIT", self.text)
        self.assertIn("restore_worktree_dist", self.text)
        self.assertIn("git -C \"$REPO_DIR\" checkout -- web/dist", self.text)

    def test_previous_binary_is_backed_up_and_rollback_restarts_service(self):
        self.assertIn('BIN_BACKUP="$TMP_DIR/pony-sentry-server.previous"', self.text)
        self.assertIn("rollback_previous_binary", self.text)
        self.assertIn('systemctl restart "$SERVICE"', self.text)
        self.assertIn("DEPLOY_OK", self.text)

    def test_embedded_token_binary_is_not_world_readable(self):
        self.assertIn('BIN_MODE="${PONY_SENTRY_BIN_MODE:-0750}"', self.text)
        self.assertIn('-m "$BIN_MODE" -o root -g "$SERVICE_GROUP"', self.text)
        self.assertIn("-perm /004", self.text)
        self.assertIn("umask 077", self.text)
        self.assertIn('chmod 0600 "$BIN_SRC"', self.text)

    def test_script_must_run_as_non_root_build_user(self):
        self.assertIn('if [ "$(id -u)" -eq 0 ]; then', self.text)
        self.assertIn("请以普通部署用户运行本脚本", self.text)

    def test_authenticated_read_endpoints_use_short_lived_cookie_session(self):
        for path in ("/api/v1/issues?", "/api/v1/projects", "/api/v1/traces?limit=100"):
            self.assertIn(path, self.text)
        self.assertIn('/api/v1/auth/session', self.text)
        self.assertIn('-b "$COOKIE_JAR"', self.text)
        self.assertIn('Cookie 会话 PATCH 未强制 CSRF', self.text)

    def test_build_does_not_inject_static_admin_token(self):
        self.assertIn('pnpm build', self.text)
        self.assertNotIn('VITE_PONY_SENTRY_TOKEN', self.text)
        self.assertIn('grep -qF "$ADMIN_TOKEN"', self.text)
        self.assertIn('控制台产物仍包含 ADMIN_TOKEN', self.text)


if __name__ == "__main__":
    unittest.main()
