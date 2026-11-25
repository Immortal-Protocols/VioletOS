"""Rose's Codex conductor with VioletOS's apps.

Everything Rose's conversation does stays hers (rose/codex_conductor.py):
barge-in and holds, heard-text logging, her task feed and quiet-time
announcements, list/update/cancel_task, her name and voice, her life and
journal, transcripts. This subclass only:

- adds Violet's app tools: build_app (a Rose task that Violet Agent's builder
  carries out: Codex writes and builds the app in the workspace, then it is
  installed on the phone), open_app, list_apps and remove_app;
- withholds tools whose far end is Rose Agent on Android (its launcher
  customization, its app builder, its workspace page, its clipboard), and the
  phone-control tools until the phone answers screenshots and taps;
- adds a few lines of policy about VioletOS.

Violet Agent answers the app tools over the frame link (host.py, Bridge).
"""
from __future__ import annotations

import time

from rose import builder_voice, composer, customization
from rose.codex_conductor import CodexConductor, PhoneTaskError, TaskAnnouncement, tool

def _named(tool_name, description):
    """A tool whose one field is called name (rose's tool() takes name for itself)."""
    t = tool(tool_name, description, app='string')
    t['inputSchema']['properties'] = {'name': {'type': 'string'}}
    t['inputSchema']['required'] = ['name']
    return t


VIOLET_TOOLS = [
    tool('build_app', 'Build an app for the owner\'s VioletOS phone, or change one built before, and install it. '
         'Runs in the background as a task; its result is announced when it is installed.', request='string'),
    _named('open_app', 'Open an installed app on the VioletOS phone\'s screen.'),
    tool('list_apps', 'The apps installed on the VioletOS phone and the room left.'),
    _named('remove_app', 'Remove an installed app from the VioletOS phone. Only after the owner agreed to remove that app.'),
]
VIOLET_POLICY = """
Phone tasks here work on the screen the owner sees, one app at a time, by screenshots and touch. Apps on this phone
have their own on-screen keys: there is no typing into fields from outside and no Back button.
You live on the owner's VioletOS phone (a Moto G Pure running VioletOS), and run on their Mac.
When the owner asks for an app or a change to one, call build_app at once with what they asked for, in their words
(say "in Python" when they asked for Python), then say one short line. The build runs as a task: keep talking with the
owner meanwhile, report it with list_tasks if asked, amend it with update_task, stop it with cancel_task. A build is
done only when its task says installed.
open_app opens an installed app; list_apps lists them with the room left. When an install fails for lack of room,
offer to remove apps the owner no longer wants, and call remove_app only for apps the owner agreed to remove.
"""
# Their far end is Rose Agent on an Android phone.
ANDROID_ONLY = set(customization.NAMES) | set(builder_voice.NAMES) | {composer.TOOL['name'], 'copy_to_phone_clipboard'}
# The phone-control tools wait for screenshots and taps on VioletOS.
PHONE_CONTROL = {'phone_task', 'look_at_screen'}


class VioletConductor(CodexConductor):
    """Set ``bridge`` (host.Bridge) before start()."""

    bridge = None
    phone_control = False

    def _conversation_tools(self):
        hidden = ANDROID_ONLY | (set() if self.phone_control else PHONE_CONTROL)
        return [t for t in super()._conversation_tools() if t['name'] not in hidden] + VIOLET_TOOLS

    def _policy(self):
        # The instructions for withheld tools go with them: Rose must not offer what this phone cannot do.
        policy = super()._policy()
        for line in (customization.POLICY_LINE, builder_voice.POLICY_LINE, composer.POLICY_LINE):
            policy = policy.replace(line.strip() + '\n', '')
        hidden = ANDROID_ONLY | (set() if self.phone_control else PHONE_CONTROL | {'update_task', 'cancel_task'})
        # One screen here: never two app tasks at once.
        words = sorted(hidden) + ['Never copy on your own', 'two apps'] + ([] if self.phone_control else ['Do not describe a screen', 'phone action'])
        kept = [l for l in policy.split('\n') if not any(w in l for w in words)]
        return '\n'.join(kept) + VIOLET_POLICY

    def start_phone(self, app, goal, **kw):
        # Rose's life can start app work by itself: never when phone control is off.
        if not self.phone_control:
            raise PhoneTaskError('phone_control_unavailable', 'This phone cannot be operated by the agent.')
        # One screen: one app task at a time (a follow-up to the same app joins its task).
        with self._lock:
            other = next((s for s in self.sessions.values() if s.task and not s.closed and not s.cancelled.is_set()
                          and s.task.status == 'running' and s.app.casefold() != str(app).casefold()), None)
        if other is not None:
            raise PhoneTaskError('one_screen', f'This phone shows one app at a time: wait for the task in {other.app}, '
                                               'or stop it first.')
        return super().start_phone(app, goal, **kw)

    # ------------------------------------------------------------- tools
    def _conversation_tool(self, name, a, page=None, call_id=None):
        if name == 'build_app':
            task = self.start_build(str(a.get('request', '')))
            return {'ok': True, 'task_id': task.id, 'status': task.status}, None
        if name in ('open_app', 'list_apps', 'remove_app'):
            return self.bridge.call(name, a, timeout=20), None
        if name == 'update_task' and self._build(a.get('task_id')):
            return {'ok': self.update_build(int(a['task_id']), str(a.get('instruction', '')))}, None
        if name == 'cancel_task' and self._build(a.get('task_id')):
            return {'ok': self.cancel_build(int(a['task_id']))}, None
        return super()._conversation_tool(name, a, page=page, call_id=call_id)

    # ------------------------------------------------------------- builds
    def _build(self, task_id):
        try:
            t = self.tasks.get(int(task_id))
        except (TypeError, ValueError):
            return None
        return t if t is not None and t.kind == 'build' else None

    def start_build(self, request):
        """A build is one of Rose's tasks: in her feed, listed, amended, stopped and announced like hers."""
        from rose.tasks import Task
        with self._lock:
            running = next((t for t in self.tasks.values() if t.kind == 'build' and t.status == 'running'), None)
        if running is not None:
            # One builder: more for the app being built joins that build.
            self.update_build(running.id, request)
            return running
        with self._lock:
            task = Task(next(self._ids), 'build', request)
            task.detail = 'Starting the build.'
            self.tasks[task.id] = task
        self._publish(task)
        self.bridge.send('build', task=task.id, request=request)
        return task

    def update_build(self, task_id, instruction):
        t = self._build(task_id)
        if t is None or t.status != 'running':
            return bool(t and self.start_build(instruction))
        t.updates.append(instruction)
        self.bridge.send('build_update', task=t.id, instruction=instruction)
        return True

    def cancel_build(self, task_id):
        t = self._build(task_id)
        if t is None or t.status != 'running':
            return False
        t.detail = 'Stopping.'
        self._publish(t)
        self.bridge.send('build_cancel', task=t.id)
        return True

    def build_progress(self, task_id, detail):
        t = self._build(task_id)
        if t is not None and t.status == 'running':
            t.detail = str(detail)[:200]
            t.steps.append({'n': len(t.steps) + 1, 'what': 'build', 'actions': [{'ok': True, 'result': t.detail}]})
            self._publish(t)

    def build_done(self, task_id, status, result):
        """The builder finished: the task closes, and Rose says so when nobody is talking."""
        t = self._build(task_id)
        if t is None or t.status != 'running':
            return
        t.status = status if status in ('done', 'failed', 'cancelled') else 'failed'
        t.result, t.detail, t.finished = str(result), '', time.time()
        self._publish(t)
        s = self.sessions['conversation']
        self._announcements.put(TaskAnnouncement(self.conversation, t.id, t.result, s.thread, s.turn))

    def _publish(self, task):
        self.bus.emit('task', task=self.task_snapshot(task))
