"""Launch the seL4 QEMU target with an SDL view of guest display memory."""
from pathlib import Path
import sys
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
IMAGE = ROOT / "build/sel4/qemu"
sys.path.insert(0,str(ROOT/"system/package-manager"))
from manage import launch_catalogs, acknowledge_rollback

RAM_SIZE = 1024**3

def machine(image=IMAGE, watchdog=False, scenario=0, disk=None, catalogs=(None,None)):
    cmd = ["qemu-system-arm", "-machine", "virt,gic-version=3,virtualization=on",
           "-cpu", "max", "-accel", "tcg", "-m", "1G", "-smp", "1", "-display", "none",
           "-monitor", "none", "-nic", "none", "-no-reboot"]
    if disk is not None:
        cmd += ["-global", "virtio-mmio.force-legacy=false",
                "-drive", f"if=none,id=violetdisk,format=raw,file={str(disk).replace(',', ',,')}",
                "-device", "virtio-blk-device,drive=violetdisk,bus=virtio-mmio-bus.0"]
    for name, address, entry in [("zImage", "0x45000000", ""), ("handoff.dtb", "0x47880000", ""),
                                  ("entry.bin", "0x44000000", ",cpu-num=0")]:
        cmd += ["-device", f"loader,file={image/name},addr={address},force-raw=on{entry}"]
    for address,path in zip(("0x49400000","0x49500000"),catalogs):
        if path:
            if path.stat().st_size>1024**2:raise ValueError("catalog exceeds boot window")
            cmd+=["-device",f"loader,file={str(path).replace(',', ',,')},addr={address},force-raw=on"]
    for address, value in [("0x49001000",1),("0x49002000",85),("0x49000000", int(watchdog)), ("0x49000010", scenario)]:
        cmd += ["-device", f"loader,addr={address},data={value},data-len=4"]
    return cmd

def finish(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()

def main(viewer):
    with tempfile.TemporaryDirectory(prefix="violet-qemu-") as tmp:
        ram = Path(tmp)/"guest.ram"
        with ram.open("wb") as stream:
            stream.truncate(RAM_SIZE)
        disk = IMAGE / "stopwatch.disk"
        try:
            with disk.open("xb") as stream:
                stream.truncate(1024**2)
        except FileExistsError:
            pass
        state_dir=IMAGE/"installed"
        catalogs=launch_catalogs(state_dir)
        selected=catalogs[0]
        last_source=1
        cmd = machine(disk=disk,catalogs=catalogs) + ["-object", f"memory-backend-file,id=ram,size=1G,mem-path={ram},share=on",
                           "-machine", "memory-backend=ram", "-serial", "stdio"]
        guest = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL)
        try:
            window = subprocess.Popen([viewer, ram], stdout=guest.stdin)
            try:
                while window.poll() is None and guest.poll() is None:
                    if selected:
                        with ram.open("rb") as mem:
                            mem.seek(0x09300008)
                            generation=int.from_bytes(mem.read(4),"little")
                            source=int.from_bytes(mem.read(4),"little")
                        if generation and source!=last_source and source in (0,2):
                            acknowledge_rollback(state_dir,selected.stem,source)
                            selected=catalogs[1] if source==2 else None
                            last_source=source
                    time.sleep(.05)
                if guest.poll() is not None:
                    raise SystemExit(f"QEMU exited while the window was open (status {guest.returncode})")
                return window.returncode
            finally:
                finish(window)
        finally:
            guest.stdin.close()
            finish(guest)

if __name__=="__main__":
    signal.signal(signal.SIGTERM, lambda *_: exit(0))
    try:
        raise SystemExit(main(sys.argv[1] if len(sys.argv) > 1 else ROOT/"build/sel4/viewer"))
    except KeyboardInterrupt:
        pass
