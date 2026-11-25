"""The VioletOS phone, as Rose's phone (rose/realphone.py, rose/vision_phone.py).

Rose operates apps by sight on phones without an accessibility tree: her
VisionSession takes screenshots, taps and swipes at coordinates, and verifies
each step in the next screenshot, under her own guards (no sends, purchases or
deletions; the owner's Stop; step and time budgets). This gives that session a
VioletOS workspace: screenshots of the panel and touches through the Open
Runtime, carried by Violet Agent to the phone's agent (apps/open/agent/src/
control.rs). VioletOS has one screen and no hidden workspace, so Rose works on
the screen the owner sees, one app at a time. Typing into fields and Android's
Back are not available; apps here are built with their own on-screen keys.

It also answers what Rose's server and her life ask of a phone: the installed
apps, look() for look_at_screen, and the controller's state.
"""
from __future__ import annotations

import threading
import time

from rose.phone import Screen
from rose.realphone import RealPhone
from rose.vision_phone import VisionSession

MODEL = "Moto G Pure (VioletOS)"
APPS_TTL_S = 30


class Link:
    """What Rose reads of a phone link."""

    def __init__(self):
        self.connected = False
        self.authenticated = False
        self.model = MODEL

    def call(self, action, timeout=10.0, **args):
        return {"status": "unsupported", "reason": f"{action} is not available on VioletOS"}


class Workspace:
    """One task's hold on the phone's screen, in the shape of Rose's ScopedWorkspace."""

    def __init__(self, phone: "VioletPhone"):
        self.phone = phone
        self.package = None
        self.bound_id = None
        self.task_id = None
        self.uncertain = False
        self.visual_only = True
        self.stopped_by_owner = False
        self.stop_requested = threading.Event()
        self.in_use = lambda: False
        # No IME typing or IME search on VioletOS: Rose's vision session refuses them plainly.
        self.capabilities = {"visionText": False, "visionSearch": False, "multiSession": False, "maxSessions": 1}
        self.session = None

    def start(self, package):
        if self.stop_requested.is_set():
            return {"status": "stopped_by_owner"}
        if self.package and package != self.package:
            return {"status": "background_unavailable", "reason": "This session belongs to another app."}
        self.package = package
        result = self.phone.call("open_app", app=package)
        if result.get("status") == "opened":
            self.bound_id = self.task_id = f"violet-{package}"
            self.session = {"sessionId": self.bound_id, "targetPackage": package}
        return result

    def screenshot(self):
        return self.phone.call("screenshot")

    def command(self, action, **params):
        if self.stop_requested.is_set():
            return {"status": "stopped_by_owner"}
        if action == "screen_gesture":
            return self.phone.call("gesture", points=params.get("points"))
        return {"status": "unsupported", "reason": f"{action} is not available on VioletOS"}

    def request_stop(self):
        self.stop_requested.set()

    def get_state(self):
        return {"status": "ready" if self.bound_id else "idle", "deviceLocked": False}

    def _check_stop(self, state):
        return None  # the owner stops a task with cancel_task (or by voice); there is no separate Stop control

    def close(self, reason="closed"):
        self.bound_id = self.task_id = None
        return {"status": "closed"}

    def touch(self):
        pass

    def _send(self, operation, params):
        return dict(self.capabilities) if operation == "capabilities" else {"status": "unsupported"}


class VioletPhone:
    """Rose's phone for VioletOS. ``call(action, **args)`` reaches the phone (host.Bridge)."""

    real = True
    vision = True
    vision_control = True
    _decode = staticmethod(RealPhone._decode)

    def __init__(self, call):
        self._call = call
        self.listeners = []
        self.link = Link()
        self.version = 0
        self._sessions = {}
        self._lock = threading.Lock()
        self._apps = []
        self._apps_at = 0.0
        self.look_status = ""
        self.workspace = Workspace(self)  # for capability questions; tasks get their own

    # ------------------------------------------------------------ the link
    @property
    def connected(self) -> bool:
        return self.link.connected

    def set_connected(self, online: bool) -> None:
        self.link.connected = self.link.authenticated = bool(online)
        self.version += 1
        for listener in list(self.listeners):
            try:
                listener("connected" if online else "disconnected", {})
            except Exception:
                pass

    def call(self, action, **args):
        if not self.connected:
            return {"status": "offline", "reason": "The phone is not connected."}
        result = self._call("phone", {"action": action, "args": args})
        return result if isinstance(result, dict) else {"status": "failed"}

    # ------------------------------------------------------------ apps
    def apps(self):
        if time.monotonic() - self._apps_at > APPS_TTL_S or not self._apps:
            result = self.call("list_apps")
            if result.get("status") == "ok":
                self._apps = [{"packageName": a.get("package"), "label": a.get("label") or a.get("package")}
                              for a in result.get("apps", []) if a.get("package")]
                self._apps_at = time.monotonic()
        return list(self._apps)

    def resolve_app(self, name):
        """(package, label) for an app the owner named, or (None, None)."""
        want = str(name or "").strip().casefold()
        for a in self.apps():
            if want in (a["packageName"].casefold(), a["label"].casefold()):
                return a["packageName"], a["label"]
        for a in self.apps():
            if want and (want in a["label"].casefold() or a["label"].casefold() in want):
                return a["packageName"], a["label"]
        return None, None

    def app_label(self, package):
        return next((a["label"] for a in self._apps if a["packageName"] == package), package)

    def _controller(self, operation, params):
        return {"status": "idle" if operation == "get_state" else "stopped", "deviceLocked": False}

    # ------------------------------------------------------------ sessions
    def session(self, name, on_screen=False, independent=False, visual=False):
        with self._lock:
            s = self._sessions.get(name)
            if s is None:
                s = self._sessions[name] = VisionSession(self, name, on_screen=False, workspace=Workspace(self))
                s._background = True
            return s

    def close_session(self, name):
        with self._lock:
            self._sessions.pop(name, None)

    # ------------------------------------------------------------ the screen
    def capture(self):
        shot = self.call("screenshot")
        return self._decode(shot)

    def look(self, view=None):
        image, info = self.capture()
        self.look_status = info.get("status", "")
        return image

    def screenshot_png(self, marks=False, view=None) -> bytes:
        import io
        image = self.look()
        if image is None:
            return b""
        out = io.BytesIO()
        image.save(out, format="PNG")
        return out.getvalue()

    def screen(self, view=None):
        return Screen("phone", MODEL, [])

    def ui_tree(self, view=None):
        return "Screenshot only on VioletOS: use look_at_screen."

    def summary(self):
        where = MODEL + ("" if self.connected else " (not connected right now)")
        return f"Phone: the owner's {where}. Rose sees it through screenshots and works in its apps by touch."

    def snapshot(self):
        return {"version": self.version, "real": True, "connected": self.connected, "model": MODEL, "view": "main",
                "route": [], "workspace": None, "screen": "phone", "title": MODEL, "toast": None, "banner": None,
                "last_tap": None, "size": [720, 1600], "elements": [],
                "status": {"time": time.strftime("%-I:%M"), "dnd": False, "wifi": True, "airplane": False}}

    def background_status(self):
        return {"available": False, "reason": "VioletOS has one screen"}

    def background_available(self):
        return False

    def workspace_app(self, view=None):
        return None

    def notify(self, app, title, text):
        pass

    def reset(self):
        with self._lock:
            self._sessions.clear()

    def close(self):
        self.reset()
