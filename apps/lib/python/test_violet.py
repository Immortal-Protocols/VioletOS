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

    def test_lifecycle_values(self):
        # EVENT_LIFECYCLE (7): LOW_MEMORY (4) is not a focus event, nor is an unknown value.
        raw = lambda value: violet._EVENT.pack(0, 7, 0, 0, 0, value)
        kinds = [violet._decode(raw(v)).kind for v in (1, 2, 3, 4, 9)]
        self.assertEqual(kinds, ["focus", "blur", "quit", "lowmemory", "other"])
        self.assertFalse(violet._decode(raw(4)).focus)

    def test_timeout_ticks(self):
        ticks = [e for e in violet.events(timeout=0.01) if e is None]
        self.assertGreaterEqual(len(ticks), 3)

    def test_save_and_load(self):
        self.assertEqual(violet.load("score", 0), 0)
        violet.save("score", {"home": 3})
        self.assertEqual(violet.load("score"), {"home": 3})

    def test_data_names_on_the_phone(self):
        # As on the phone: /data/APP or /data/APP.KEY, never over 16 characters,
        # never a name another app owns, and two keys never share a file.
        fake, argv = violet.FAKE, sys.argv
        try:
            violet.FAKE = False
            sys.argv = ["/apps/scorekeeper"]
            self.assertEqual(violet._file("scorekeeper"), "scorekeeper")
            self.assertEqual(violet._file("best"), "scorekeeper.best")
            self.assertEqual(violet._file("scorekeeper.best"), "scorekeeper.best")
            a, b = violet._file("highscore"), violet._file("highscore2")
            self.assertNotEqual(a, b)
            for n in (a, b):
                self.assertTrue(n.startswith("scorekeeper.") and len(n) <= 16, n)
            sys.argv = ["/bin/python3", "/apps/weatherforec/main.py"][1:]
            for key in ("city", "forecast-cache", "x" * 40):
                n = violet._file(key)
                self.assertTrue(n.startswith("weatherforec.") and len(n) <= 16, n)
            sys.argv = ["/apps/neighbourhoodw"]
            with self.assertRaises(ValueError):
                violet._file("longer-key")
        finally:
            violet.FAKE, sys.argv = fake, argv


if __name__ == "__main__":
    unittest.main()
