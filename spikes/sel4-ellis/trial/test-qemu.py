"""Run the combined ARM image; inspect physical pixels over the QEMU monitor."""
import argparse, pathlib, subprocess, struct, re, sys, time
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--overlap", action="store_true", help="reserve loader staging RAM and check that boot is rejected")
negative = parser.parse_args().overlap
repo=pathlib.Path(__file__).resolve().parents[3]
base=repo/"build/spike2"
out=base/"trial-fixture"
report=(base/"live-board.txt").read_text()
# Reconstruct exactly the measured reg/tag bytes on top of the factory tree.
dt=out/"handoff.dtb"
dt.write_bytes((base/"factory-base.dtb").read_bytes())
for path,hexdata in re.findall(r"^DT (\S+)\n((?: [\da-f ]+\n)+)",report,re.M):
    node,prop=path.rsplit("/",1)
    subprocess.run(["fdtput","-p","-t","bx",dt,node,prop,*hexdata.split()],check=True)
if negative:
    subprocess.run(["fdtput","-p","-t","x",dt,"/reserved-memory/test-overlap","reg","0","43000000","0","1000"],check=True)
# Linux ARM entry provides the DT address in r2.
cmd=["qemu-system-arm","-machine","virt,gic-version=3,virtualization=on","-cpu","max","-m","1G","-smp","1","-display","none","-serial","none","-monitor","stdio","-no-reboot"]
for name,addr,pc in [("zImage","0x45000000",""),("handoff.dtb","0x47880000",""),("entry.bin","0x44000000",",cpu-num=0")]:
    cmd+=["-device",f"loader,file={out/name},addr={addr},force-raw=on{pc}"]
with subprocess.Popen(cmd,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True) as proc:
    time.sleep(2)
    log,_=proc.communicate("xp /1wx 0x79467000\nxp /1wx 0x79450000\ninfo registers\nquit\n",timeout=6)
(out/("rejection.log" if negative else "monitor.log")).write_text(log)
if negative:
    for address in ("79467000","79450000"):
        if not re.search(address+r":\s+0x00000000",log): sys.exit("Expected untouched framebuffer")
    print("Overlapping firmware reservation rejected before either marker.")
    sys.exit(0)
if not re.search(r"79467000:\s+0xffffffff",log) or not re.search(r"79450000:\s+0xff00ff00",log):
    sys.exit("Missing marker. See "+str(out/"monitor.log"))
print("Loader and user-task markers verified in QEMU memory.")
