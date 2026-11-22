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
parser.add_argument("--stack",choices=["handset-ui","lifecycle","removed","recovered","save-radio","restore-radio"])
parser.add_argument("--library",choices=["install","open","remove","absent","radio-install","radio-rollback","radio-reboot","install-second"])
parser.add_argument("--wifi",choices=["exercise","save","restore","live","async","weather"])
parser.add_argument("--state-dir",type=Path)
parser.add_argument("--fault-id",type=int,default=7)
args=parser.parse_args()
enabled=args.watchdog_enabled
repo=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(repo/"tools"))
from run import IMAGE, machine
scratch=tempfile.TemporaryDirectory()
out=Path(scratch.name)
image=IMAGE/"isolation-test" if args.probe else IMAGE
from manage import launch_catalogs
catalogs=launch_catalogs(args.state_dir) if args.state_dir else (None,None)
from contextlib import ExitStack
from emulator_endpoint import endpoint_peer,connect_endpoint
resources=ExitStack()
def untrusted_transport():
    import socket
    if args.wifi=="async":
        (out/"started").touch()
        deadline=time.monotonic()+8
        while not (out/"release").exists() and time.monotonic()<deadline:time.sleep(.02)
    client,server=socket.socketpair()
    server.sendall(b"HTTP/1.0 200 OK\r\n\r\n12345678-abcd-4abc-8def-0123456789ab")
    server.shutdown(socket.SHUT_WR)
    resources.callback(server.close)
    return client
peer=resources.enter_context(endpoint_peer(connect_endpoint if args.wifi in ("live","weather") else untrusted_transport)) if args.wifi else None
cmd=machine(peer=peer,catalogs=catalogs,image=image,watchdog=enabled, scenario=modes.index(args.spi), disk=args.disk)+["-serial","pty","-qmp","stdio"]
if args.stack=="recovered" or args.library=="radio-rollback":
    cmd+=["-device","loader,addr=0x49300014,data=1,data-len=4"]
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
    if args.wifi or args.library:
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
        if args.library:
            def library():
                tap(260,560)
                check_label(48,"INSTALLED",0xffd1b0fe)
            if args.library.startswith("radio-"):
                def status():
                    path=out/"catalog.raw"
                    request("pmemsave",dict(val=0x49300008,size=12,filename=str(path)))
                    return struct.unpack("<III",path.read_bytes())
                if args.library!="radio-reboot":
                    library()
                    check_label(248,"RADIO",0xffd1b0fe)
                    tap(1150)
                    expected=(3,3,7) if args.library=="radio-rollback" else (2,3,0)
                    deadline=time.monotonic()+10
                    while status()!=expected:
                        if time.monotonic()>deadline:raise AssertionError((expected,status()))
                        time.sleep(.05)
                else:assert status()==(1,3,0),status()
                if args.library == "radio-rollback":
                    # Observe the in-place rollback before stopping this guest.
                    # The next scenario cold-boots the same disk and exercises
                    # Radio through the UI with a fresh virtual UART.
                    print("Guest service package transaction:", args.library, status())
                    sys.exit(0)
                time.sleep(.5)
                tap(1140)
                check_label(104,"VIRTUAL RADIO OFF",0xffeeeaf8)
                tap(1140)
                check_label(104,"VIRTUAL RADIO ONLINE",0xffeeeaf8)
                print("Guest service package transaction:",args.library,status())
                sys.exit(0)
            library()
            if args.library in ("install","install-second"):
                check_label(248,"SECOND" if args.library=="install-second" else "NOTEBOOK",0xffd1b0fe)
                tap(1150)
                time.sleep(1)
                library()
            tap(440);tap(440)
            if args.library=="install-second":tap(440)
            if args.library=="absent":
                # NEXT skips the removed slot 11, so the page after WI-FI is SECOND (12).
                assert read_label(62,6,0xffeeeaf8,2)=="SECOND"
            else:
                title="SECOND" if args.library=="install-second" else "NOTEBOOK"
                assert read_label(62,len(title),0xffeeeaf8,2)==title
                if args.library=="remove":
                    tap(860);time.sleep(1);library()
                    tap(440);tap(440)
                    assert read_label(62,6,0xffeeeaf8,2)=="SECOND"
                else:
                    tap(650)
                    assert read_label(24,8,0xffeeeaf8,2)=="NOTEBOOK"
                    check_label(70,"SAVED FILE" if args.library in ("install","install-second") else "LOADED FILE",0xffeeeaf8)
                    tap(1280)
            print("Guest application library:",args.library)
            sys.exit(0)
        ink,accent=0xffeeeaf8,0xffd1b0fe
        tap(680)
        if args.wifi=="restore":
            assert read_label(101,10,ink,2)=="GUEST WIFI"
            check_label(130,"CONNECTED",ink)
            sys.exit(0)
        check_label(130,"DISCONNECTED",ink)
        tap(1080)
        check_label(141,"CONNECT FIRST",accent)
        if args.wifi=="save":
            tap(330,560)
            assert read_label(101,10,ink,2)=="GUEST WIFI"
        tap(820)
        check_label(130,"CONNECTED",ink)
        if args.wifi=="save": sys.exit(0)
        if args.wifi=="weather":
            # Home -> Applications -> NEXT past WATCH, WIFI, MUSIC and SOUNDS (empty slots are skipped) -> Open.
            tap(1340); time.sleep(.3)
            tap(260,560); time.sleep(.3)
            for _ in range(4): tap(440)
            time.sleep(.3)
            assert read_label(62,7,ink,2)=="WEATHER",read_label(62,7,ink,2)
            tap(650); time.sleep(.5)
            check_label(14,"WEATHER",accent)
            tap((118+22+6)*4,150)  # OAKLAND: second row, first column
            for _ in range(150):
                if read_label(104,17,accent)=="LIVE FROM WTTR.IN": break
                time.sleep(.1)
            check_label(104,"LIVE FROM WTTR.IN",accent)
            assert read_label(26,7,ink,2)=="OAKLAND"
            temperature=read_label(48,4,ink,4).strip("? ")
            assert temperature.endswith("F") and temperature[:-1].lstrip("-").isdigit(),temperature
            print("Live Oakland weather from guest-verified TLS:",temperature,read_label(82,18,ink,2).rstrip("? "))
            sys.exit(0)
        test_start=time.monotonic()
        tap(1080)
        if args.wifi=="async":
            assert (out/"started").exists()
            def wait_for_screen(test):
                deadline=time.monotonic()+2
                while not test():
                    if time.monotonic()>=deadline:raise AssertionError("screen transition timed out while radio pending")
                    time.sleep(.03)
            tap(1340)
            wait_for_screen(lambda:read_label(62,6,ink,4)=="VIOLET")
            tap(920)
            wait_for_screen(lambda:value()==0)
            tap(920)
            wait_for_screen(lambda:(value() or 0)>0)
            assert time.monotonic()-test_start<5.5, "transport timeout preceded stopwatch"
            (out/"release").touch()
            print("Touch, app switching and display worked during outstanding radio I/O.")
            sys.exit(0)
        if args.wifi=="live":
            import uuid
            for _ in range(100):
                if read_label(141,15,accent)=="UUID FROM HTTPS": break
                time.sleep(.1)
            check_label(141,"UUID FROM HTTPS",accent)
            displayed=read_label(152,18,ink,2)+read_label(168,18,ink,2)
            assert uuid.UUID(displayed).version==4,displayed
            print("Live UUID from guest-verified TLS:",displayed)
            sys.exit(0)
        time.sleep(.3)
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
        check_label(130,"RADIO OFF",ink)
        tap(1340);tap(1140)  # Restore the saved radio setting before reboot.
        print("Guest rejected unauthenticated endpoint data and reported link and radio failures.")
        sys.exit(0)
    if args.stack:
        def words(address,count=1):
            path=out/"status.raw"
            request("pmemsave",dict(val=address,size=4*count,filename=str(path)))
            return struct.unpack("<"+"I"*count,path.read_bytes())
        if args.stack=="handset-ui":
            assert words(0x4930000c)==(1,), "handset catalog did not activate"
            tap(920)
            assert value()==0, "launcher failed to open stopwatch"
            tap(920)
            before=value()
            assert before is not None and before>0
            tap(1420)
            assert value() is None, "Home did not regain focus"
            tap(920)
            assert value()>before, "stopwatch lost elapsed time while in background"
            print("Handset app catalog passed Start, Home and reopen with running time.")
            sys.exit(0)
        if args.stack in ("save-radio","restore-radio"):
            if args.stack=="save-radio": tap(1140)
            assert words(0x49001004)==(1,), "launcher preference was lost"
            tap(920)
            assert value()==args.expect, "launcher changed stopwatch storage"
            sys.exit(0)
        if args.stack=="recovered":
            if args.fault_id in (2,10):
                tap(680 if args.fault_id==10 else 920)  # The stopwatch loads on demand.
            if args.fault_id in (13,14):
                # Sounds and Weather load on demand too: Apps -> NEXT past STOPWATCH
                # WI-FI and MUSIC (and Sounds, for Weather) -> OPEN.
                tap(260,560); time.sleep(.3)
                for _ in range(3 if args.fault_id==13 else 4): tap(440)
                time.sleep(.3)
                tap(650)
            for _ in range(140):
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
