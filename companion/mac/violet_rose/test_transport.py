"""PhoneTransport without Rose or a phone: python3 -m unittest companion/mac/violet_rose/test_transport.py"""
import os
import sys
import unittest

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from violet_rose import frames, transport  # noqa: E402


class PhoneTransportTest(unittest.TestCase):
    def setUp(self):
        self.sent = []
        frames.put = lambda kind, data: self.sent.append(kind)
        frames.event = lambda **e: self.sent.append(e)
        self.t = transport.PhoneTransport()
        self.reports = []
        self.t.on_client = self.reports.append

    def test_offline_speech_counts_as_played(self):
        self.t.send_audio(3, np.zeros(480, dtype=np.float32))
        self.assertEqual(self.reports, [{"type": "played", "seg": 3, "samples": 480, "done": True, "dropped": True}])

    def test_a_stop_answered_in_time_reaches_rose(self):
        self.t.set_online(True)
        self.t.send_audio(1, np.zeros(480, dtype=np.float32))
        self.t.send_control({"type": "stop"})
        self.t.report({"type": "stopped", "seg": 1, "samples": 100})
        self.assertEqual(self.reports, [{"type": "stopped", "seg": 1, "samples": 100}])

    def test_a_late_stop_never_cuts_the_next_reply(self):
        self.t.set_online(True)
        self.t.send_audio(1, np.zeros(480, dtype=np.float32))
        self.t.send_control({"type": "stop"})
        self.t.send_audio(2, np.zeros(480, dtype=np.float32))  # Rose gave up waiting and speaks on
        self.t.report({"type": "stopped", "seg": -1, "samples": 0})  # the phone thaws and answers the old stop
        self.assertEqual(self.reports, [])
        self.t.report({"type": "played", "seg": 2, "samples": 480, "done": True})
        self.assertEqual(self.reports, [{"type": "played", "seg": 2, "samples": 480, "done": True}])

    def test_microphone_arrives_in_listener_frames(self):
        got = []
        self.t.on_mic = got.append
        self.t.mic((np.ones(700, dtype="<i2") * 1000).tobytes())
        self.t.mic((np.ones(400, dtype="<i2") * 1000).tobytes())
        self.assertEqual([len(f) for f in got], [512, 512])

    def test_dropping_the_phone_stops_what_was_playing(self):
        self.t.set_online(True)
        self.t.set_online(False)
        self.assertEqual(self.reports, [{"type": "stopped", "seg": -1, "samples": 0, "reason": "disconnected"}])


if __name__ == "__main__":
    unittest.main()
