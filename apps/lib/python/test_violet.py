"""The violet Python module against its fake screen (make test-unit; no phone needed)."""
import os
import sys
import tempfile
import unittest

os.environ["VIOLET_FAKE"] = "1"
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import violet  # noqa: E402


def pixel(s, x, y):
    at = (y * violet.W + x) * 4
    b, g, r, _ = s.fb[at:at + 4]
    return (r << 16) | (g << 8) | b


class Violet(unittest.TestCase):
    def setUp(self):
        self.data = tempfile.TemporaryDirectory()
        os.environ["VIOLET_FAKE_DATA"] = self.data.name
        os.environ["VIOLET_FAKE_EVENTS"] = ""
        violet._swallow = False

    def tearDown(self):
        self.data.cleanup()

    def test_shapes(self):
        s = violet.Screen()
        s.fill(violet.BACKGROUND)
        s.rect(100, 100, 200, 100, violet.ACCENT, radius=30)
        self.assertEqual(pixel(s, 200, 150), violet.ACCENT & 0xFFFFFF)
        self.assertEqual(pixel(s, 101, 101), violet.BACKGROUND & 0xFFFFFF)  # a rounded corner
        s.circle(500, 500, 40, violet.GREEN)
        self.assertEqual(pixel(s, 500, 500), violet.GREEN & 0xFFFFFF)
        self.assertEqual(pixel(s, 545, 500), violet.BACKGROUND & 0xFFFFFF)
        s.rect(-50, 1590, 100, 50, violet.RED)  # clipped, no error
        self.assertEqual(pixel(s, 0, 1599), violet.RED & 0xFFFFFF)

    def test_text_draws_and_measures(self):
        s = violet.Screen()
        s.fill(violet.BACKGROUND)
        w = s.text(40, 300, "Hello", size=48, bold=True)
        self.assertGreater(w, 60)
        self.assertEqual(w, violet.text_width("Hello", 48, True))
        if violet._load_faces():
            inked = sum(pixel(s, x, y) != violet.BACKGROUND & 0xFFFFFF for x in range(40, 40 + w) for y in range(300, 360))
            self.assertGreater(inked, 100)

    def test_events_and_home_taps(self):
        os.environ["VIOLET_FAKE_EVENTS"] = "100,60;300,700"
        s = violet.Screen()
        s.header("Test")
        kinds = [(e.kind, e.x, e.y) for e in violet.events()]
        # The Home button's press and lift are not the app's; the other tap is.
        self.assertEqual(kinds, [("focus", 0, 0), ("down", 300, 700), ("up", 300, 700)])

    def test_timeout_ticks(self):
        ticks = [e for e in violet.events(timeout=0.01) if e is None]
        self.assertGreaterEqual(len(ticks), 3)

    def test_save_and_load(self):
        self.assertEqual(violet.load("score", 0), 0)
        violet.save("score", {"home": 3})
        self.assertEqual(violet.load("score"), {"home": 3})


if __name__ == "__main__":
    unittest.main()
