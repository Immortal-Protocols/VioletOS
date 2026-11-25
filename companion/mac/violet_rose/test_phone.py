"""Rose's vision session on the VioletOS phone, with a fake phone (no Rose server, no device):
    build/rose-env/bin/python companion/mac/violet_rose/test_phone.py"""
import base64
import io
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "roseos", "prototypes", "voice-phone-demo"))
sys.path.insert(0, os.path.join(HERE, ".."))

from PIL import Image  # noqa: E402
from violet_rose.phone import VioletPhone  # noqa: E402


def png(color):
    out = io.BytesIO()
    Image.new("RGB", (360, 800), color).save(out, format="PNG")
    return base64.b64encode(out.getvalue()).decode()


class FakeDevice:
    """Answers the phone commands as apps/open/agent/src/control.rs does."""

    def __init__(self):
        self.calls, self.shots = [], 0

    def __call__(self, name, args):
        action, a = args["action"], args["args"]
        self.calls.append((action, a))
        if action == "list_apps":
            return {"status": "ok", "apps": [{"package": "coin-flip", "label": "Coin Flip"}]}
        if action == "screenshot":
            self.shots += 1
            return {"status": "ok", "imageBase64": png((20, 14, 30)), "width": 360, "height": 800,
                    "displayWidth": 720, "displayHeight": 1600, "packageName": "coin-flip", "screenshotId": f"violet-{self.shots}"}
        if action == "open_app":
            return {"status": "opened", "observation": self(name, {"action": "screenshot", "args": {}})}
        if action == "gesture":
            return {"status": "gesture_dispatched"}
        return {"status": "unsupported"}


class VioletPhoneTest(unittest.TestCase):
    def setUp(self):
        self.device = FakeDevice()
        self.phone = VioletPhone(self.device)
        self.phone.set_connected(True)

    def test_apps_resolve_by_name_or_title(self):
        self.assertEqual(self.phone.resolve_app("coin flip"), ("coin-flip", "Coin Flip"))
        self.assertEqual(self.phone.resolve_app("Coin-Flip"), ("coin-flip", "Coin Flip"))
        self.assertEqual(self.phone.resolve_app("Weather"), (None, None))

    def test_look_at_screen(self):
        self.assertEqual(self.phone.look().size, (360, 800))

    def test_rose_vision_session_opens_taps_and_verifies(self):
        view = self.phone.session("app-1", independent=True, visual=True)
        view.workspace.package = "coin-flip"
        opened = view.execute("open_app", {"app": "coin-flip"})
        self.assertTrue(opened["ok"], opened)
        frame = view.frame_id
        tapped = view.execute("tap_point", {"frame_id": frame, "x": 500, "y": 600, "target": "the Flip button",
                                            "label": "Flip", "icon": "", "effect": "interact"})
        self.assertTrue(tapped["ok"], tapped)
        self.assertIn(("gesture", {"points": [360, 960]}), self.device.calls)  # 0-1000 coordinates to display pixels
        self.assertNotEqual(view.frame_id, frame)  # a fresh screenshot verifies the tap
        reused = view.execute("tap_point", {"frame_id": frame, "x": 1, "y": 1, "target": "x", "label": "", "icon": "",
                                            "effect": "interact"})
        self.assertFalse(reused["ok"])  # never two inputs on one frame
        typed = view.execute("type_text", {"frame_id": view.frame_id, "text": "hi", "replace": True, "purpose": "text"})
        self.assertFalse(typed["ok"])  # no typing into fields on VioletOS

    def test_sends_stay_with_the_owner(self):
        view = self.phone.session("app-2", independent=True, visual=True)
        view.execute("open_app", {"app": "coin-flip"})
        sent = view.execute("tap_point", {"frame_id": view.frame_id, "x": 500, "y": 500, "target": "send button",
                                          "label": "Send", "icon": "send arrow", "effect": "interact"})
        self.assertFalse(sent["ok"])

    def test_offline_phone_answers_plainly(self):
        self.phone.set_connected(False)
        self.assertIsNone(self.phone.look())


if __name__ == "__main__":
    unittest.main()
