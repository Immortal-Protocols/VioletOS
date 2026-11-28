"""App manifests as the image build packs them (tools/build_initfs.py) and the
Agent's build wraps them (tools/agent_build.py), for runtime/open/policy.rs.
Part of `make test-unit` (well under a second)."""
import struct
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import build_initfs  # noqa: E402


def folder(base, name, manifest=None):
    d = Path(base) / name
    d.mkdir()
    if manifest is not None:
        (d / "manifest.txt").write_text(manifest)
    return d


class Manifests(unittest.TestCase):
    def test_checked_in_manifests_pass(self):
        # Every manifest in the tree packs, for the image and the test images.
        for top in ["apps/open", "apps/shield-demo", "targets/qemu/open-test-apps", "targets/qemu/memory-test-apps"]:
            apps = [a for a in (ROOT / top).iterdir() if a.is_dir()]
            text = build_initfs.pack_manifests(apps)
            for a in apps:
                self.assertEqual(f"@{a.name}\n" in text, (a / "manifest.txt").exists(), a)
        text = build_initfs.pack_manifests([a for a in (ROOT / "apps/open").iterdir() if a.is_dir()])
        self.assertIn("@settings\ncaps: net wifi-admin system audio-out\ndata: networks relay\n", text)
        self.assertIn("@home\ncaps: net system\n", text)

    def test_pack_format(self):
        with tempfile.TemporaryDirectory() as t:
            apps = [folder(t, "b", "caps: net, mic # the microphone\n\n# nothing\n"), folder(t, "a", "caps:\n"), folder(t, "c")]
            self.assertEqual(build_initfs.pack_manifests(apps), "@a\ncaps: \n@b\ncaps: net mic\n")
        # serve: (runtime/open/loopback.rs serve_words) is packed as written,
        # in the form tests/integration's World hands manifest.rs.
        with tempfile.TemporaryDirectory() as t:
            apps = [folder(t, "srv", "caps: net\nserve: apps, lan # loopback and the wildcard\n")]
            self.assertEqual(build_initfs.pack_manifests(apps), "@srv\ncaps: net\nserve: apps lan\n")
        self.assertEqual(build_initfs.read_manifest("serve: lan", "x"), ["serve: lan"])
        for text in ["serve: world", "serve: apps internet", "serves: lan"]:
            with self.assertRaises(SystemExit, msg=text):
                build_initfs.read_manifest(text, "x")

    def test_mistakes_fail_the_build(self):
        bad = ["caps: network", "cap: net", "net", "data: ../agent", "data: .grants", "data: a/b", "data: other", "data: other.keys"]
        for text in bad:
            with self.assertRaises(SystemExit, msg=text):
                build_initfs.read_manifest(text, "x", {"other"})
        self.assertEqual(build_initfs.read_manifest("data: networks relay.saved", "x", {"other"}), ["data: networks relay.saved"])
        # Reserved /tmp names start with '.' (runtime/open/policy.rs path_rule).
        self.assertEqual(build_initfs.read_manifest("tmp: .ota-chunk, .locomo", "x"), ["tmp: .ota-chunk .locomo"])
        for text in ["tmp: plain", "tmp: ./x", "tmp: .a/b", "tmp: ."]:
            with self.assertRaises(SystemExit, msg=text):
                build_initfs.read_manifest(text, "x")

    def test_reserved_tmp_names_are_declared_by_both_ends(self):
        text = build_initfs.pack_manifests([a for a in (ROOT / "apps/open").iterdir() if a.is_dir()])
        self.assertIn("@settings\ncaps: net wifi-admin system audio-out\ndata: networks relay\ntmp: .ota-chunk\n", text)
        for app in ("agent", "music"):
            self.assertIn("tmp: .locomo\n", text.split(f"@{app}\n", 1)[1].split("@", 1)[0], app)
        for app in ("agent", "music"):
            src = (ROOT / f"apps/open/{app}/src/locomo.rs").read_text()
            self.assertNotIn('"/tmp/locomo', src)
            self.assertIn('"/tmp/.locomo.', src)

    def test_envelope(self):
        blob = build_initfs.envelope("caps: net camera # scans codes\n", b"#!/bin/python3\n")
        self.assertTrue(blob.startswith(b"#!/bin/python3\n") and blob.endswith(b"VMF1"))
        n = struct.unpack("<H", blob[-6:-4])[0]
        self.assertEqual(blob[-6 - n:-6], b"caps: net camera\n")
        self.assertEqual(blob[:-6 - n], b"#!/bin/python3\n")
        with self.assertRaises(SystemExit):
            build_initfs.envelope("caps: root", b"")


if __name__ == "__main__":
    unittest.main()
