"""Boot the seL4 fixture: touch-controller faults and the watchdog, Home answering a tap, and isolation probes."""
from pathlib import Path
import sys
import tempfile
import argparse
import os, subprocess, time, json, struct, re, tty
parser=argparse.ArgumentParser()
parser.add_argument("--watchdog-enabled", action="store_true")
modes = ["normal","delayed","drop-before","drop-after","stuck","setup","reset1","reset2"]
parser.add_argument("--spi", choices=modes, default="normal")
parser.add_argument("--probe",choices=["touch","storage","dma","scanout","peer","radio","radio-dma"])
args=parser.parse_args()
enabled=args.watchdog_enabled
repo=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(repo/"tools"))
from run import IMAGE, machine
scratch=tempfile.TemporaryDirectory()
out=Path(scratch.name)
image=IMAGE/"isolation-test" if args.probe else IMAGE
sys.path.insert(0,str(repo/"targets/qemu"))
cmd=machine(image=image,watchdog=enabled, scenario=modes.index(args.spi))+["-serial","pty","-qmp","stdio"]
if args.probe:
    address={"touch":0x09003000,"storage":0x0a000000,"dma":0x0b000000,"scanout":0x08000000,"peer":0x06100000,"radio":0x09001000,"radio-dma":0x09002000}[args.probe]
    cmd+=["-device",f"loader,addr=0x49300000,data={address},data-len=4"]
p=subprocess.Popen(cmd,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,bufsize=1)
try:
    while True:
        line=p.stdout.readline()
        if not line: raise RuntimeError("QEMU exited")
        if "/dev/" in line: port=re.search(r'(/dev/\S+)',line)[1]
        if '"QMP"' in line: break
    def request(name,args):
        p.stdin.write(json.dumps(dict(execute=name,arguments=args))+'\n');p.stdin.flush()
        while True:
            line=p.stdout.readline()
            if not line: raise RuntimeError("QEMU closed")
            if "/dev/" in line:
                global port
                port=re.search(r"(/dev/\S+)",line)[1]
                continue
            if not line.startswith("{"): raise RuntimeError(line)
            msg=json.loads(line)
            if "return" in msg: return msg["return"]
            if "error" in msg: raise RuntimeError(msg)
    request("qmp_capabilities",{})
    if args.probe:
        filename=out/"fault.raw"
        for _ in range(60):
            request("pmemsave",dict(val=0x49300004,size=4,filename=str(filename)))
            if struct.unpack("<I",filename.read_bytes())[0]==address: break
            time.sleep(.05)
        else: raise AssertionError(f"missing UI VM fault at {address:#x}")
        print(f"seL4 denied UI access to {args.probe} at {address:#x}")
        sys.exit(0)
    serial=os.open(port,os.O_RDWR|os.O_NOCTTY);tty.setraw(serial)
    def settle(test, what, timeout=10):
        # The screen catches up with a tap later on a slow machine (CI).
        deadline=time.monotonic()+timeout
        while not test():
            if time.monotonic()>=deadline: raise AssertionError(what)
            time.sleep(.05)
    def tap(y,x=360):
        os.write(serial,bytes([9,x>>4,y>>4,((x&15)<<4)|(y&15),1,1]))
        time.sleep(.18)
        os.write(serial,bytes(6));time.sleep(.18)
    def log():
        path=out/"log.raw"
        request("pmemsave",dict(val=0x49220000,size=4096,filename=str(path)))
        raw=path.read_bytes();n=struct.unpack_from("<I",raw,4)[0];data=raw[8:]
        if n<=len(data): return data[:n].decode(errors="replace")
        k=n%len(data);return (data[k:]+data[:k]).decode(errors="replace")
    time.sleep(1)
    if args.spi not in ("stuck","reset1","reset2"):
        # Home comes up on its own; a tap on Settings must reach it through
        # Touch, Runtime and the Open Runtime.
        import home_grid
        grid=home_grid.apps((IMAGE/"initfs.cpio").read_bytes())
        settle(lambda:"home: ready" in log(), "Home did not start", 60)
        x,y=home_grid.icon(grid.index("settings"), grid)
        time.sleep(1)
        tap(y,x)
        settle(lambda:"settings: ready" in log(), "a tap on Home did not open Settings", 30)
        print("Touch reached Home: a tap opened Settings.")
    sample=out/"watchdog.raw"
    request("pmemsave",dict(val=0x49000000,size=40,filename=str(sample)))
    mode,_,reload,_,scenario,injections,resets,reads,invalid,polls=struct.unpack("<10I",sample.read_bytes())
    expected_counts={"normal":(0,0,0), "delayed":(1,0,0), "drop-before":(2,2,2),
                     "drop-after":(1,1,2), "stuck":(5,5,5), "setup":(2,2,0), "reset1":(5,5,0), "reset2":(5,5,0)}
    assert (injections,resets,reads)==expected_counts[args.spi],(injections,resets,reads)
    assert scenario==modes.index(args.spi) and invalid==0
    if args.spi=="delayed": assert polls>0
    print(f"{args.spi}: {injections} faults, {resets} controller recoveries, {reads} SRAM reads, {invalid} register violations")
    assert mode==int(enabled) and reload==(0x1971 if enabled else 0)
    print(f"Watchdog register fixture verified (enabled={enabled}).")
finally:
    p.kill();p.wait()
