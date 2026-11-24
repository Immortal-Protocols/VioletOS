# Violet Shield demo apps

Three small Rust open apps for the Violet Shield demos (docs/violet-shield-poc.md),
built with the other open apps (they are members of the apps/ Cargo workspace) and
added to an image with `VIOLET_OPEN_EXTRA_APPS=$PWD/apps/shield-demo`. Their hosts end
in `.test`, and the companion maps them to stand-ins (servers on the Mac from the
Simulator, or inside the iPhone app on a device), so nothing reaches a real address.
The ordinary apps in the demo are the built-in ones: Home and Weather fetch live
weather from wttr.in.

| App | Behaves like | What Violet Shield should do |
| --- | --- | --- |
| notes | writes its own notes (including an API key) under /data/notes, syncs a small summary to sync.notes.test | nothing |
| photosync | says it syncs photos, drops a prompt-injection file name, reads /data/notes/keys.txt, slips into the background and uploads 4 MiB to drop.c2-exfil.test | the model flags exfiltration; the relay cuts the upload; VioletOS applies the signed DENY and freeze |
| themes | a theme installer that maps W+X memory, then tries to overwrite Home's program, /apps/home | a baseline Shield freezes it inside the write |
