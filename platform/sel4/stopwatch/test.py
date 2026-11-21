"""Boot the seL4 fixture and read each stopwatch digit from the framebuffer."""
from pathlib import Path
import sys
import tempfile
import argparse
import os, subprocess, time, json, struct, re, tty
parser=argparse.ArgumentParser()
parser.add_argument("--watchdog-enabled", action="store_true")
modes = ["normal","delayed","drop-before","drop-after","stuck","setup","reset1","reset2"]
parser.add_argument("--spi", choices=modes, default="normal")
parser.add_argument("--disk",type=Path)
parser.add_argument("--checkpoint",choices=["save","restore","reset"])
parser.add_argument("--expect",type=int, default=0)
parser.add_argument("--probe",choices=["touch","storage","dma","scanout","peer","radio","radio-dma"])
parser.add_argument("--stack",choices=["lifecycle","removed","recovered","save-radio","restore-radio"])
parser.add_argument("--wifi",choices=["exercise","save","restore","live"])
parser.add_argument("--state-dir",type=Path)
parser.add_argument("--fault-id",type=int,default=7)
args=parser.parse_args()
enabled=args.watchdog_enabled
repo=Path(__file__).resolve().parents[3]
sys.path.insert(0,str(repo/"tools"))
from run import IMAGE, machine
scratch=tempfile.TemporaryDirectory()
out=Path(scratch.name)
image=IMAGE/"isolation-test" if args.probe else IMAGE
from manage import launch_catalogs
catalogs=launch_catalogs(args.state_dir) if args.state_dir else (None,None)
from contextlib import ExitStack
from emulator_endpoint import endpoint_peer,fetch_uuid
resources=ExitStack()
responses=[]
def endpoint_response():
    if args.wifi=="live":
        data=fetch_uuid()
    else:
        data=b"12345678-abcd-4abc-8def-0123456789ab" if len(responses)<2 else b"invalid"
    responses.append(data)
    return data
peer=resources.enter_context(endpoint_peer(endpoint_response)) if args.wifi else None
cmd=machine(peer=peer,catalogs=catalogs,image=image,watchdog=enabled, scenario=modes.index(args.spi), disk=args.disk)+["-serial","pty","-qmp","stdio"]
if args.probe:
    address={"touch":0x09003000,"storage":0x0a000000,"dma":0x0b000000,"scanout":0x08000000,"peer":0x06100000,"radio":0x09001000,"radio-dma":0x09002000}[args.probe]
    cmd+=["-device",f"loader,addr=0x49300000,data={address},data-len=4"]
p=subprocess.Popen(cmd,pass_fds=(peer,) if peer is not None else (),stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,bufsize=1)
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
    digits=[tuple(map(int,x.split(','))) for x in re.findall(r'\{([\d, ]+)\}',(repo/'src/glyphs.h').read_text())[:10]]
    def snapshot():
        filename=out/"display.raw"
        request("pmemsave",dict(val=0x79450000,size=2880*1600,filename=str(filename)))
        return filename.read_bytes()
    def pixel(frame,x,y):
        return struct.unpack_from('<I',frame,y*2880+x*4)[0]
    def value():
        frame=snapshot()
        result=''
        for pos in [0,1,3,4,6]:
            x=(14+24*pos)*3
            cols=tuple(sum((pixel(frame,x+i*12,139*4+j*16)==0xffd1b0fe)<<j for j in range(7)) for i in range(5))
            if cols not in digits: return None
            result+=str(digits.index(cols))
        return int(result[:2])*600+int(result[2:4])*10+int(result[4])
    def tap(y,x=360):
        os.write(serial,bytes([9,x>>4,y>>4,((x&15)<<4)|(y&15),1,1]))
        time.sleep(.18)
        os.write(serial,bytes(6));time.sleep(.18)
    time.sleep(1)
    if args.wifi:
        font=(repo/"src/glyphs.h").read_text()
        alphabet=re.search(r'alphabet\[\] = "([^"]+)"',font)[1]
        glyphs=[tuple(map(int,x.split(','))) for x in re.findall(r'\{([\d, ]+)\}',font)]
        def read_label(y,length,color,scale=1):
            frame=snapshot()
            actual=""
            for n in range(length):
                cols=tuple(sum((pixel(frame,(14+(n*6+x)*scale)*3,(y+j*scale)*4)==color)<<j for j in range(7)) for x in range(5))
                actual+=alphabet[glyphs.index(cols)] if cols in glyphs else "?"
            return actual
        def check_label(y,expected,color):
            actual=read_label(y,len(expected),color)
            assert actual==expected,(expected,actual)
        ink,accent=0xffeeeaf8,0xffd1b0fe
        tap(680)
        if args.wifi=="restore":
            check_label(130,"CONNECTED",ink)
            sys.exit(0)
        check_label(130,"DISCONNECTED",ink)
        tap(1080)
        check_label(141,"CONNECT FIRST",accent)
        tap(820)
        check_label(130,"CONNECTED",ink)
        if args.wifi=="save": sys.exit(0)
        for n in range(1 if args.wifi=="live" else 2):
            tap(1080)
            for _ in range(75):
                if read_label(141,13,accent)=="UUID FROM HTT": break
                time.sleep(.1)
            check_label(141,"UUID FROM HTTPS",accent)
            assert len(responses)==n+1, "one HTTPS request per tap"
            displayed=read_label(152,18,ink,2)+read_label(168,18,ink,2)
            assert displayed==responses[-1].decode("ascii"),displayed
            if args.wifi=="live":
                print("Live endpoint UUID decoded from the QEMU screen:",responses[-1].decode("ascii"))
                sys.exit(0)
        tap(1080)
        check_label(141,"HTTPS TEST FAILED",accent)
        assert read_label(152,18,ink,2)+read_label(168,18,ink,2)==" "*36
        request("set_link",dict(name="wifi",up=False))
        tap(1080)
        time.sleep(.7)
        check_label(141,"NO GATEWAY REPLY",accent)
        request("set_link",dict(name="wifi",up=True))
        tap(820)
        check_label(130,"DISCONNECTED",ink)
        tap(1340);tap(1140);tap(680)
        check_label(130,"RADIO OFF",ink)
        tap(820)
        check_label(141,"CHECK RADIO ON HOME",accent)
        tap(1340);tap(1140)  # Restore the saved radio setting before reboot.
        print("QEMU Wi-Fi setup, two displayed UUIDs, invalid response, link failure, disconnect and radio-off verified.")
        sys.exit(0)
    if args.stack:
        def words(address,count=1):
            path=out/"status.raw"
            request("pmemsave",dict(val=address,size=4*count,filename=str(path)))
            return struct.unpack("<"+"I"*count,path.read_bytes())
        if args.stack in ("save-radio","restore-radio"):
            if args.stack=="save-radio": tap(1140)
            assert words(0x49001004)==(1,), "launcher preference was lost"
            tap(920)
            assert value()==args.expect, "launcher changed stopwatch storage"
            sys.exit(0)
        if args.stack=="recovered":
            if args.fault_id in (2,10):
                tap(680 if args.fault_id==10 else 920)  # The stopwatch loads on demand.
            for _ in range(80):
                if words(0x49300008,3)==(2,2,args.fault_id): break
                time.sleep(.05)
            else: raise AssertionError(("failed recovery",words(0x49300008,3)))
            time.sleep(.3)
            from manage import acknowledge_rollback
            acknowledge_rollback(args.state_dir,catalogs[0].stem,2)
            if args.fault_id!=7:
                tap(920)
                assert value()==0
                tap(920)
                assert value()>0
                print("Recovered and started stopwatch after fault in component",args.fault_id)
                sys.exit(0)
        if args.stack=="removed":
            home=snapshot()
            tap(920)
            assert snapshot()==home and value() is None
        else:
            tap(1140)
            assert words(0x49001004)==(1,), "radio did not disable"
            tap(1140)
            assert words(0x49001004)==(0,), "radio did not reenable"
            tap(1420)
            assert words(0x49002004)==(1,) and len(set(snapshot()[::4]))==1, "display did not sleep"
            tap(920)
            assert words(0x49002004)==(0,) and value() is None, "first touch must wake only"
            tap(920)
            assert value()==0, "stopwatch did not launch"
            tap(920)
            running=value()
            tap(1420)
            assert value() is None, "Home did not switch apps"
            tap(920)
            assert value()>running, "background stopwatch did not advance"
            tap(100,530)
            assert value() is None, "quit did not return Home"
            tap(920)
            assert value()==0, "relaunch retained private state"
        print("QEMU guest passed:",args.stack)
        sys.exit(0)
    if args.checkpoint:
        tap(920)
        assert value()==args.expect, ("restored value",value(),args.expect)
        if args.checkpoint=="save":
            tap(920); time.sleep(.5); tap(920)
            saved=value()
            time.sleep(.25)
            assert saved>0 and value()==saved
            print("Checkpoint",saved)
        elif args.checkpoint=="reset":
            tap(1140)
            assert value()==0
        sys.exit(0)
    if args.spi in ("stuck","reset1","reset2"):
        # Decode the actual status line through the same bitmap font as the UI.
        text=(repo/"src/glyphs.h").read_text()
        alphabet=re.search(r'alphabet\[\] = "([^"]+)"',text)[1]
        glyphs=[tuple(map(int,x.split(','))) for x in re.findall(r'\{([\d, ]+)\}',text)]
        expected={"stuck":"FW DATA 000080 S1 00000001",
                  "reset1":"R1 RESET PAGE S00000001 C00013000",
                  "reset2":"R2 RESET PAGE S00000001 C00013000"}[args.spi]
        frame=snapshot()
        for n,ch in enumerate(expected):
            cols=tuple(sum((pixel(frame,(14+6*n+i)*3,(32+j)*4)==0xffeeeaf8)<<j
                           for j in range(7)) for i in range(5))
            assert cols==glyphs[alphabet.index(ch)],(n,ch,cols)
        print("Framebuffer verified:",expected)
    else:
        tap(920)
        assert value()==0,"first tap did not open stopwatch"
        tap(920); time.sleep(.4)
        a=value();time.sleep(.6);b=value()
        assert a is not None and b is not None and b>a>0,(a,b)
        tap(920)
        paused=value();time.sleep(.5)
        assert paused==value() and paused>=b
        tap(1140);assert value()==0
        print(f"Framebuffer verified: 0.0, {a/10:.1f}, {b/10:.1f}, paused at {paused/10:.1f}, reset to 0.0.")
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
    p.kill();p.wait();resources.close()
