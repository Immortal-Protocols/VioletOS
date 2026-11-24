# Violet Shield demo apps

Four small Python programs for `make test-shield` and the demo recording
(docs/violet-shield-poc.md). They run unmodified under the Open Runtime; the
hosts they use end in `.test` and are mapped to local stand-ins on the Mac, so
nothing reaches a real address.

| App | Behaves like | What Violet Shield should do |
| --- | --- | --- |
| notes | writes its own notes (including an API key) under /data/notes, syncs a small summary to its own server | nothing |
| weather | fetches a forecast from api.weather.test | nothing |
| photosync | says it syncs photos, then reads /data/notes/keys.txt, drops a prompt-injection file name, slips into the background and uploads 4 MiB to drop.c2-exfil.test | model flags exfiltration; relay blocks the host; VioletOS freezes the app mid-upload on a signed Shield |
| themes | a theme installer that maps W+X memory and writes into /apps/launcher | Tier-0 rule: frozen on the first write into another app's code |
