"""Prepare pinned upstream sources, build seL4 and package the selected target."""
import argparse
import fcntl
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/"tools"))
PORT = ROOT / "platform/sel4"
BUILD = ROOT / "build/sel4"
FACTORY = ROOT / "build/xt2163dl/S3RHS32.20-42-13-2-3-2/stock"
KERNEL = ("seL4-17.0.0", "https://codeload.github.com/seL4/seL4/tar.gz/refs/tags/17.0.0",
          "cda7844ba516ba8920ba1f24ec8ab98302f0527007c1ec0cfe294aa46443b0fe")
DTC = ("dtc-1.7.2", "https://codeload.github.com/dgibson/dtc/tar.gz/refs/tags/v1.7.2",
       "8f1486962f093f28a2f79f01c1fd82f144ef640ceabe555536d43362212ceb7c")

def source(package, destination):
    name,url,digest = package
    if destination.is_dir():
        return
    cache = BUILD / "downloads"
    cache.mkdir(parents=True,exist_ok=True)
    archive = cache / (name+".tar.gz")
    with tempfile.TemporaryDirectory(dir=destination.parent) as tmp:
        if not archive.exists():
            download = Path(tmp)/"download"
            subprocess.run(["curl","-fLsS","--retry","2",url,"-o",download],check=True)
            if hashlib.sha256(download.read_bytes()).hexdigest()!=digest:
                sys.exit("Unexpected download checksum for "+name)
            download.replace(archive)
        if hashlib.sha256(archive.read_bytes()).hexdigest()!=digest:
            sys.exit("Invalid cached archive: "+str(archive))
        subprocess.run(["tar","xf",archive,"-C",tmp],check=True)
        (Path(tmp)/name).rename(destination)

def update(path, data):
    if not path.exists() or path.read_bytes()!=data:
        path.write_bytes(data)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target",choices=["qemu","moto"])
    target=parser.parse_args().target
    out=BUILD/target
    out.mkdir(parents=True,exist_ok=True)
    with (out/"build.lock").open("a+") as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        build_target(target,out)

def build_target(target,out):
    ndk=Path(os.environ.get("ANDROID_NDK_HOME",Path.home()/"Library/Android/sdk/ndk/28.2.13676358"))
    bins=list(ndk.glob("toolchains/llvm/prebuilt/*/bin/clang"))
    if not bins:
        sys.exit("Set ANDROID_NDK_HOME to the Android NDK 28.2.13676358 directory.")
    env=os.environ.copy()
    py=ROOT/"build/sel4/venv/bin/python3"
    if not py.exists():
        sys.exit("Run make setup once to install the kernel build's Python dependencies.")
    env.update(PATH=os.pathsep.join([str(py.parent),str(bins[0].parent),env["PATH"]]),
               CMAKE_POLICY_VERSION_MINIMUM="3.5")
    def run(*args):
        subprocess.run(args,env=env,check=True)
    stock=Path(os.environ.get("VIOLET_STOCK_BOOT",FACTORY/"boot.img")).resolve()
    if target=="moto" and not stock.is_file():
        sys.exit("Set VIOLET_STOCK_BOOT to the XT2163DL factory boot.img (see README).")
    src=out/"upstream"
    source(KERNEL,src)
    source(DTC,BUILD/DTC[0])
    bear=("bearssl-0.6","https://bearssl.org/bearssl-0.6.tar.gz","6705bba1714961b41a728dfc5debbe348d2966c117649392f8c8139efc83ff14")
    source(bear,BUILD/bear[0])
    if target == "moto":
        supplicant = ("wpa_supplicant-2.11", "https://w1.fi/releases/wpa_supplicant-2.11.tar.gz",
                      "912ea06f74e30a8e36fbb68064d6cdff218d8d591db0fc5d75dee6c81ac7fc0a")
        source(supplicant, BUILD/supplicant[0])
        lwip = ("lwip-STABLE-2_2_1_RELEASE", "https://codeload.github.com/lwip-tcpip/lwip/tar.gz/refs/tags/STABLE-2_2_1_RELEASE",
                "ce0b7461c0ad9602c376f0bf07c5eb7253b48c7bf66f011c6bf3e2a96731c539")
        source(lwip, BUILD/lwip[0])
    board=PORT/"board"
    plat=src/"src/plat/ellis"
    plat.mkdir(exist_ok=True)
    run(py,board/"gic-single-core.py",src)
    for name in ["config.cmake","kernel-overlay.dts","moto-radio-memory.dts"]:
        update(plat/name,(board/name).read_bytes())
    wifi = plat / "wifi"
    wifi.mkdir(exist_ok=True)
    update(wifi/"monitor.S", (board/"wifi/monitor.S").read_bytes())
    includes = src / "include/arch/arm/arch/object/ellis_wifi"
    includes.mkdir(exist_ok=True)
    for name in ("windows.h", "invoke.h"):
        update(includes/name, (board/"wifi"/name).read_bytes())
    patch = board / "wifi/aarch32.patch"
    # Idempotent for both a clean pinned source and an already-patched build.
    if subprocess.run(["git", "apply", "--reverse", "--check", patch], cwd=src,
                      stderr=subprocess.DEVNULL).returncode:
        subprocess.run(["git", "apply", patch], cwd=src, check=True)
    constants=Path("sel4/plat/api/constants.h")
    headers=src/"libsel4/sel4_plat_include"
    (headers/"ellis"/constants).parent.mkdir(parents=True,exist_ok=True)
    update(headers/"ellis"/constants,(headers/"imx8mm-evk"/constants).read_bytes())
    if target=="qemu":
        update(plat/"factory-base.dts",(board/"qemu.dts").read_bytes())
        run("dtc","-q","-I","dts","-O","dtb","-o",out/"handoff.dtb",board/"qemu.dts")
    else:
        from phone_image import factory_dtb
        update(out/"base.dtb",factory_dtb(stock.read_bytes()))
        update(plat/"factory-base.dts",subprocess.check_output(["dtc","-q","-I","dtb","-O","dts",out/"base.dtb"]))
    kernel=out/"kernel"
    run("cmake","-GNinja","-S",src,"-B",kernel,"-DCMAKE_TOOLCHAIN_FILE="+str(src/"llvm.cmake"),
        "-DTRIPLE=arm-none-eabi","-DPYTHON3="+str(py),"-DKernelPlatform=ellis",
        "-DVioletQemuBoard="+("ON" if target=="qemu" else "OFF"),"-DKernelSel4Arch=aarch32","-DKernelIsMCS=OFF","-DKernelPrinting=OFF",
        "-DKernelDebugBuild=OFF","-DKernelVerificationBuild=OFF","-DKernelArmExportVCNTUser=ON",
        # The Open Runtime programs the physical timer (PPI 30) to wake itself,
        # so it can block between events (docs/8ms-frame-pipeline-rfc.md).
        "-DKernelArmExportPTMRUser=ON",
        # Linux ARM programs read their thread pointer from TPIDRURO; the Open Runtime sets it.
        "-DKernelArmTLSReg=tpidruro")
    generated=["include/sel4/"+f for f in ["shared_types_gen.h","syscall.h","invocation.h"]]
    generated+=["include/interfaces/sel4_client.h","arch_include/arm/sel4/arch/invocation.h",
                "sel4_arch_include/aarch32/sel4/sel4_arch/invocation.h",
                "sel4_arch_include/aarch32/sel4/sel4_arch/types_gen.h"]
    run("ninja","-C",kernel,"kernel.elf",*["libsel4/"+f for f in generated])
    run(py,PORT/"loader/build.py",target,stock)

if __name__=="__main__":
    main()
