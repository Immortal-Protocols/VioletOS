"""Boot the seL4 fixture and read each stopwatch digit from the framebuffer."""
from pathlib import Path
import sys
import argparse
import os, subprocess, time, json, struct, re, tty
parser=argparse.ArgumentParser()
parser.add_argument("--watchdog-enabled", action="store_true")
modes = ["normal","delayed","drop-before","drop-after","stuck","setup","reset1","reset2"]
parser.add_argument("--spi", choices=modes, default="normal")
args=parser.parse_args()
enabled=args.watchdog_enabled
repo=Path(__file__).resolve().parents[3]
sys.path.insert(0,str(repo/"tools"))
from run import IMAGE, machine
out=IMAGE
cmd=machine(watchdog=enabled, scenario=modes.index(args.spi))+["-serial","pty","-qmp","stdio"]
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
    def tap(y):
        x=360
        os.write(serial,bytes([9,x>>4,y>>4,((x&15)<<4)|(y&15),1,1]))
        time.sleep(.18)
        os.write(serial,bytes(6));time.sleep(.18)
    time.sleep(1)
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
    p.kill();p.wait()
