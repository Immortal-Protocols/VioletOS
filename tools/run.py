"""Launch the seL4 QEMU target with an SDL view of guest display memory."""
from pathlib import Path
import sys
from emulator_endpoint import endpoint_peer
from net_relay import Relay as NetRelay
import os
import binascii
import hashlib
import json
import re
import selectors
from qemu_power import Monitor
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
IMAGE = ROOT / "build/sel4/qemu"
sys.path.insert(0,str(ROOT/"system/package-manager"))
from manage import launch_catalogs, acknowledge_rollback

RAM_SIZE = 1024**3
# The emulator's Wi-Fi: two open networks and a WPA2 one (password "violet-home").
DEFAULT_NETWORKS = ["VIOLET LAB", "GUEST WIFI", {"name": "VIOLET HOME", "password": "violet-home"}]

def machine(image=IMAGE, watchdog=False, scenario=0, disk=None, catalogs=(None,None),peer=None):
    network="guestport" if peer is not None else "wifi"
    cmd = ["qemu-system-arm", "-machine", "virt,gic-version=3,virtualization=on",
           "-cpu", "max", "-accel", "tcg", "-m", "1G", "-smp", os.environ.get("VIOLET_CORES", "4"), "-display", "none",
           "-monitor", "none", "-global","virtio-mmio.force-legacy=false",
           "-netdev","user,id=wifi,restrict=on","-device",
           f"virtio-net-device,netdev={network},bus=virtio-mmio-bus.8,mac=52:54:00:56:00:01", "-no-reboot"]
    cmd+=["-object","rng-random,id=entropy,filename=/dev/urandom","-device","virtio-rng-device,rng=entropy,bus=virtio-mmio-bus.16"]
    if peer is not None:
        cmd+=["-netdev","hubport,id=guestport,hubid=0",
              "-netdev","hubport,id=routerport,hubid=0,netdev=wifi",
              "-netdev",f"socket,id=httpspeer,fd={peer}",
              "-netdev","hubport,id=httpsport,hubid=0,netdev=httpspeer"]
    if disk is not None:
        cmd += [
                "-drive", f"if=none,id=violetdisk,format=raw,file={str(disk).replace(',', ',,')}",
                "-device", "virtio-blk-device,drive=violetdisk,bus=virtio-mmio-bus.0"]
        volume = Path(str(disk) + ".packages")
        try:
            with volume.open("xb") as stream:
                stream.truncate(4 * 1024**2)
        except FileExistsError:
            pass
        cmd += ["-drive", f"if=none,id=packages,format=raw,file={str(volume).replace(',', ',,')}",
                "-device", "virtio-blk-device,drive=packages,bus=virtio-mmio-bus.24"]
    for name, address, entry in [("zImage", "0x45000000", ""), ("handoff.dtb", "0x47880000", ""),
                                  ("entry.bin", "0x44000000", ",cpu-num=0")]:
        cmd += ["-device", f"loader,file={image/name},addr={address},force-raw=on{entry}"]
    # The open apps' file system image, read by the Open Runtime.
    initfs=image/"initfs.cpio"
    if initfs.exists():
        cmd+=["-device",f"loader,file={str(initfs).replace(',', ',,')},addr=0x4a000000,force-raw=on"]
    inbox=os.environ.get("VIOLET_INBOX")
    if inbox:
        from package_format import inspect
        source=Path(inbox).resolve()
        inspect(source.read_bytes())
        cmd+=["-device",f"loader,file={str(source).replace(',', ',,')},addr=0x49600000,force-raw=on"]
    for address,path in zip(("0x49400000","0x49500000"),catalogs):
        if path:
            if path.stat().st_size>1024**2:raise ValueError("catalog exceeds boot window")
            cmd+=["-device",f"loader,file={str(path).replace(',', ',,')},addr={address},force-raw=on"]
    # Discoverable virtual access points. Profile keys survive scan reordering.
    # An entry is a name (an open network) or {"name": ..., "password": ...} (WPA2).
    networks=json.loads(os.environ.get("VIOLET_NETWORKS",json.dumps(DEFAULT_NETWORKS)))
    if not isinstance(networks,list) or len(networks)>8:
        raise ValueError("VIOLET_NETWORKS must list up to eight networks")
    names=[n["name"] if isinstance(n,dict) else n for n in networks]
    if any(not isinstance(n,str) or not re.fullmatch(r"[A-Z0-9 -]{1,16}",n) for n in names):
        raise ValueError("VIOLET_NETWORKS must contain up to eight displayable SSIDs")
    ids=[binascii.crc32(n.encode()) or 2 for n in names]
    if len(set(ids))!=len(ids) or 1 in ids:raise ValueError("duplicate or reserved network identity")
    for i,(network,name,identity) in enumerate(zip(networks,names,ids)):
        record=identity.to_bytes(4,"little")+name.encode().ljust(16,b"\0")
        password=network.get("password") if isinstance(network,dict) else None
        if password is not None:
            if not 8<=len(password)<=63:raise ValueError(f"{name}: a WPA2 password has 8 to 63 characters")
            pmk=hashlib.pbkdf2_hmac("sha1",password.encode(),name.encode(),4096,32)
            key=(1).to_bytes(4,"little")+pmk
            for offset in range(0,36,4):
                value=int.from_bytes(key[offset:offset+4],"little")
                cmd+=["-device",f"loader,addr={0x49001200+i*36+offset},data={value},data-len=4"]
        for offset in range(0,20,4):
            value=int.from_bytes(record[offset:offset+4],"little")
            cmd+=["-device",f"loader,addr={0x49001100+i*20+offset},data={value},data-len=4"]
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
    with tempfile.TemporaryDirectory(prefix="violet-qemu-") as tmp, endpoint_peer() as peer:
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
        cmd = machine(disk=disk,catalogs=catalogs,peer=peer) + ["-object", f"memory-backend-file,id=ram,size=1G,mem-path={ram},share=on",
                           "-machine", "memory-backend=ram", "-serial", "stdio"]
        monitor_path=Path(tmp)/"power.sock"
        cmd+=["-qmp",f"unix:{monitor_path},server=on,wait=off"]
        # Open apps' TCP sockets go through the Mac (Radio serves them on the handset).
        net=NetRelay(ram)
        net.start()
        guest = subprocess.Popen(cmd,pass_fds=(peer,), stdin=subprocess.PIPE, stdout=subprocess.DEVNULL)
        try:
            monitor=Monitor(monitor_path,guest)
            window = subprocess.Popen([viewer, ram], stdout=subprocess.PIPE)
            inputs=selectors.DefaultSelector()
            inputs.register(window.stdout,selectors.EVENT_READ)
            asleep=False
            waiting_for_wake=False
            pending=b""

            try:
                while window.poll() is None and guest.poll() is None:
                    with ram.open("rb") as mem:
                        mem.seek(0x09002004)
                        power=int.from_bytes(mem.read(4),"little")
                    if power==2:
                        monitor.command("quit")
                        guest.wait(timeout=3)
                        return 0
                    if power==0:waiting_for_wake=False
                    if power==1 and not asleep and not waiting_for_wake:
                        monitor.command("stop");asleep=True
                    if inputs.select(.015):
                        pending+=os.read(window.stdout.fileno(),256)
                        while len(pending)>=6:
                            event,pending=pending[:6],pending[6:]
                            if asleep:
                                if not event[0]:continue
                                monitor.command("cont");asleep=False;waiting_for_wake=True
                                # Release the contact that initiated suspend before the wake press.
                                guest.stdin.write(bytes(6));guest.stdin.flush();time.sleep(.075)
                            guest.stdin.write(event);guest.stdin.flush()
                    if selected:
                        with ram.open("rb") as mem:
                            mem.seek(0x09300008)
                            generation=int.from_bytes(mem.read(4),"little")
                            source=int.from_bytes(mem.read(4),"little")
                        if generation and source!=last_source and source in (0,2):
                            acknowledge_rollback(state_dir,selected.stem,source)
                            selected=catalogs[1] if source==2 else None
                            last_source=source

                if guest.poll() is not None:
                    raise SystemExit(f"QEMU exited while the window was open (status {guest.returncode})")
                return window.returncode
            finally:
                inputs.close();monitor.close();window.stdout.close();finish(window)
        finally:
            guest.stdin.close()
            finish(guest)
            net.stop()

if __name__=="__main__":
    signal.signal(signal.SIGTERM, lambda *_: exit(0))
    try:
        raise SystemExit(main(sys.argv[1] if len(sys.argv) > 1 else ROOT/"build/sel4/viewer"))
    except KeyboardInterrupt:
        pass
