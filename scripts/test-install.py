#!/usr/bin/env python3
"""Exercise release installation in a temporary HOME without a desktop session."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parent.parent
KEY = "org.freedesktop.impl.portal.FileChooser"


class InstallationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="kestrel-install-")
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.home = root / "home with spaces"
        self.data = self.home / "data"
        self.config = self.home / "config"
        self.portals = self.config / "xdg-desktop-portal"
        self.portals.mkdir(parents=True)
        self.mocks = root / "mocks"
        self.mocks.mkdir()
        for command in ("busctl", "systemctl", "pgrep", "pkill", "update-desktop-database", "gtk-update-icon-cache"):
            mock = self.mocks / command
            mock.write_text("#!/bin/sh\nexit 0\n")
            mock.chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.home), XDG_DATA_HOME=str(self.data),
                        XDG_CONFIG_HOME=str(self.config), XDG_CURRENT_DESKTOP="Hyprland:sway",
                        XDG_CONFIG_DIRS=str(root / "system-config"), XDG_DATA_DIRS=str(root / "system-data"),
                        PATH=f"{self.mocks}:/usr/bin:/bin")
        self.bundle = root / "release"
        (self.bundle / "assets").mkdir(parents=True)
        (self.bundle / "scripts").mkdir()
        for name in ("kestrel.desktop", "kestrel.svg"):
            shutil.copy(REPO / "assets" / name, self.bundle / "assets" / name)
        shutil.copy(REPO / "scripts/install-prebuilt.sh", self.bundle / "install.sh")
        shutil.copy(REPO / "scripts/integrate.sh", self.bundle / "scripts/integrate.sh")
        (self.bundle / "kestrel").write_text("#!/bin/sh\nexit 0\n")
        (self.bundle / "VERSION").write_text("test\n")

    def run_script(self, path, *args):
        subprocess.run(["bash", str(path), *args], env=self.env, check=True, capture_output=True, text=True)

    def install(self):
        self.run_script(self.bundle / "install.sh")

    def uninstall(self):
        self.run_script(self.home / ".local/bin/kestrel-uninstall")

    def test_existing_preferences_survive_update_and_uninstall(self):
        conf = self.portals / "hyprland-portals.conf"
        original = f"# custom\n[preferred]\ndefault=hyprland;gtk\n{KEY}=gtk\norg.freedesktop.impl.portal.ScreenCast=hyprland\n\n[other]\nx=1\n"
        conf.write_text(original)
        self.install()
        self.install()
        text = conf.read_text()
        self.assertEqual(text.count(f"\n{KEY}=kestrel\n"), 1)
        self.assertEqual(text.count("# kestrel previous:"), 1)
        conf.write_text(text + "# added after install\n")
        self.uninstall()
        self.assertEqual(conf.read_text(), original + "# added after install\n")
        self.assertFalse((self.home / ".local/bin/kestrel").exists())
        self.assertFalse((self.data / "dbus-1/services/org.freedesktop.impl.portal.desktop.kestrel.service").exists())

    def test_non_systemd_session_restarts_frontend(self):
        (self.mocks / "systemctl").write_text("#!/bin/sh\nexit 1\n")
        marker = self.home / "frontend-restarted"
        (self.mocks / "pkill").write_text(f"#!/bin/sh\ntouch '{marker}'\n")
        self.install()
        self.assertTrue(marker.exists())

    def test_generic_user_preferences_are_used(self):
        conf = self.portals / "portals.conf"
        conf.write_text("[preferred]\ndefault=wlr;gtk\n")
        self.install()
        self.assertIn(f"{KEY}=kestrel", conf.read_text())
        self.assertFalse((self.portals / "hyprland-portals.conf").exists())
        desktop = (self.data / "applications/kestrel.desktop").read_text()
        self.assertIn(f'Exec="{self.home}/.local/bin/kestrel" %f', desktop)
        self.uninstall()
        self.assertEqual(conf.read_text(), "[preferred]\ndefault=wlr;gtk\n")

    def test_system_preferences_are_preserved(self):
        system = Path(self.env["XDG_DATA_DIRS"]) / "xdg-desktop-portal"
        system.mkdir(parents=True)
        (system / "sway-portals.conf").write_text(f"[preferred]\ndefault=wlr\n{KEY}=gtk\n")
        self.install()
        conf = self.portals / "hyprland-portals.conf"
        self.assertIn("default=wlr", conf.read_text())
        self.uninstall()
        self.assertEqual(conf.read_text(), f"[preferred]\ndefault=wlr\n{KEY}=gtk\n")

    def test_missing_preferred_section_and_fresh_install(self):
        conf = self.portals / "hyprland-portals.conf"
        conf.write_text("[other]\nx=1\n")
        self.install()
        self.assertIn(f"[preferred]\n{KEY}=kestrel", conf.read_text())
        self.uninstall()
        self.assertNotIn(KEY, conf.read_text())
        conf.unlink()
        self.install()
        self.assertIn(f"{KEY}=kestrel", conf.read_text())


if __name__ == "__main__":
    unittest.main()
