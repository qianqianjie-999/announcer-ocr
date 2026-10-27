import os
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class TestDeployAssets(unittest.TestCase):
    def test_official_deploy_installs_maint_and_uses_calling_user(self):
        text = (ROOT / "announce-rs-deploy" / "deploy.sh").read_text(encoding="utf-8")
        self.assertIn("announce-maint", text)
        self.assertIn("/etc/sudoers.d/announce-maint", text)
        self.assertIn("${APP_USER} ALL=(root) NOPASSWD: ${MAINT_SELF}", text)
        self.assertIn('install -m 0755 -o root -g root "${MAINT_SRC}" "${MAINT_SELF}"', text)

    def test_official_deploy_uses_repository_schedule_script(self):
        text = (ROOT / "announce-rs-deploy" / "deploy.sh").read_text(encoding="utf-8")
        self.assertIn('SCHED_SRC="${BUILD_DIR}/deploy/announce-schedule-apply"', text)
        self.assertIn('install -m 0755 -o root -g root "${SCHED_SRC}" "$SCHED_SELF"', text)

    def test_schedule_scripts_do_not_drift(self):
        canonical = (ROOT / "deploy" / "announce-schedule-apply").read_bytes()
        deployed = (ROOT / "announce-rs-deploy" / "announce-schedule-apply").read_bytes()
        self.assertEqual(canonical, deployed)

    def test_schedule_refuses_missing_config_in_apply_mode(self):
        text = (ROOT / "deploy" / "announce-schedule-apply").read_text(encoding="utf-8")
        self.assertIn('ANNOUNCE_CONF', text)
        self.assertIn("/home/debian/anntest/config.json", text)
        self.assertIn("拒绝按 ENABLED=0 兜底继续", text)

    def test_schedule_missing_explicit_config_fails_without_side_effects(self):
        script = ROOT / "deploy" / "announce-schedule-apply"
        missing = ROOT / "definitely-not-a-config.json"
        result = subprocess.run(
            ["bash", str(script), "--apply", "--conf", str(missing)],
            text=True,
            capture_output=True,
            timeout=5,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("找不到可读的 config.json", result.stderr)

    def test_maint_is_fail_closed(self):
        text = (ROOT / "deploy" / "announce-maint").read_text(encoding="utf-8")
        self.assertIn("set -euo pipefail", text)
        self.assertIn("systemctl is-active --quiet", text)
        self.assertIn("flock -n 9", text)
        self.assertNotIn('cp -a "$OPT/announce-rs" "$PRE/" 2>/dev/null', text)
        self.assertIn("回滚", text)

    def test_maint_restart_failure_is_reported_as_failure(self):
        script = ROOT / "deploy" / "announce-maint"
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp) / "app"
            fake_bin = pathlib.Path(tmp) / "bin"
            root.mkdir()
            fake_bin.mkdir()
            (root / "announce-rs").write_bytes(b"old")
            (root / "config.json").write_text("{}", encoding="utf-8")
            systemctl = fake_bin / "systemctl"
            systemctl.write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = restart ]; then exit 7; fi\n"
                "exit 0\n",
                encoding="utf-8",
            )
            systemctl.chmod(0o755)
            env = os.environ.copy()
            env.update({
                "ANNOUNCE_ROOT": str(root),
                "ANNOUNCE_BACKUP_DIR": str(pathlib.Path(tmp) / "backup"),
                "PATH": str(fake_bin) + os.pathsep + env["PATH"],
            })
            result = subprocess.run(
                ["bash", str(script), "restart-service"],
                text=True,
                capture_output=True,
                timeout=5,
                env=env,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("重启 announce-rs 失败", result.stderr)

    def test_web_service_accepts_persistent_auth_env(self):
        text = (ROOT / "deploy" / "announce-web.service").read_text(encoding="utf-8")
        self.assertIn("EnvironmentFile=-/etc/announce-rs-web.env", text)

    def test_maint_sudoers_is_a_template_not_a_hardcoded_unknown_user(self):
        text = (ROOT / "deploy" / "sudoers.announce-maint").read_text(encoding="utf-8")
        self.assertIn("__APP_USER__ ALL=(root) NOPASSWD:", text)
        self.assertNotIn("debian13 ALL=(root)", text)


if __name__ == "__main__":
    unittest.main()
