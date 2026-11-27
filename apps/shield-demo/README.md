# Violet Shield demo apps

Three small Rust open apps for the Violet Shield demos (docs/violet-shield-poc.md),
built with the other open apps (they are members of the apps/ Cargo workspace) and
added to an image with `VIOLET_OPEN_EXTRA_APPS=$PWD/apps/shield-demo`. Their hosts end
in `.test`, and the companion maps them to stand-ins (servers on the Mac from the
Simulator, or inside the iPhone app on a device), so nothing reaches a real address.
The ordinary apps in the demo are the built-in ones: Home and Weather fetch live
weather from wttr.in.

| App | Behaves like | What Violet Shield does |
| --- | --- | --- |
| notes | writes its own notes (including an API key) under /data/notes, syncs a small summary to sync.notes.test | nothing |
| photosync | says it syncs photos, drops a prompt-injection file name, reads /data/notes/keys.txt, slips into the background and uploads 4 MiB to drop.c2-exfil.test | the companion's model warns on Home. The phone itself reports it (`over-limit 7`) and, when photosync is not part of the boot image or the phone is in strict mode, quarantines it: the upload is cut off within its first few hundred KiB |
| themes | a theme installer that maps W+X memory, then tries to overwrite Home's program, /apps/home: by opening it for writing, then through a read-only descriptor (write, ftruncate) and a read-only open with O_TRUNC | the open for writing and the truncating open are refused with EACCES by built-in rule 1, and the read-only descriptor cannot write (EBADF), with no companion; in strict mode the scorer also freezes it for 30 s |

In the demo images these apps are built in, so by default the phone's rate limits and scorer only report on them;
`make test-shield-enforce` builds a strict-mode image to see them contained. See "What the phone enforces by itself"
in docs/violet-shield-poc.md. The enforcement half has host tests only so far.
