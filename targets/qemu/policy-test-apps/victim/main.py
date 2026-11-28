"""Keeps private files under its own name in /data, for the policy probe
(targets/qemu/test_system.py) to fail to see. No manifest: its own /data,
the screen and touch."""
import os

with open("/data/victim", "w") as f:
    f.write("secret\n")
with open("/data/victim.keys", "w") as f:
    f.write("api-key\n")
os.sync()
print("victim: wrote /data/victim and /data/victim.keys", flush=True)
with open("/dev/violet/sys", "wb", buffering=0) as system:
    system.write(b"home\n")
