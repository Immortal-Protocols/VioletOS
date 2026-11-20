"""Link the VioletOS first task and package it with the target kernel."""
from pathlib import Path
import sys, subprocess, struct, shlex, hashlib, os, shutil

here = Path(__file__).resolve().parent
repo = here.parents[2]
build = repo / "build/sel4"
variant, stock_path = sys.argv[1:]
app = here.parent / "stopwatch"
out = build / variant
def run(cmd):
    subprocess.run(cmd, cwd=out, check=True)

abi = out/"kernel"
probe = os.environ.get("VIOLET_TEST_ISOLATION") == "1" and variant=="qemu"
if probe:
    out=out/"isolation-test"
    out.mkdir(exist_ok=True)
    shutil.copyfile(out.parent/"handoff.dtb",out/"handoff.dtb")
line = next(s for s in subprocess.check_output(["ninja","-C",abi,"-t","commands","sel4"],text=True).splitlines() if " -c " in s and s.endswith("/sel4_bootinfo.c"))
args = shlex.split(line)
cc = Path(args[0])
ld = cc.with_name("ld.lld")
objcopy = cc.with_name("llvm-objcopy")
common = [cc,"--target=arm-none-eabi","-mcpu=cortex-a53","-marm","-mfloat-abi=soft","-Oz","-ffreestanding","-fno-builtin","-fno-stack-protector","-fno-unwind-tables","-fno-pic","-mno-unaligned-access","-ffunction-sections","-fdata-sections"]
incs = [a for a in args if a.startswith("-I") and ("libsel4" in a or "autoconf" in a or "gen_config" in a)]
defines = ["-DNDEBUG", "-DROOT_BYTES=0x40000"]
if probe: defines.append("-DVIOLET_TEST_ISOLATION")
if variant=="qemu": defines.append("-DVIOLET_QEMU")
sys.path.insert(0,str(app))
from firmware import prepare
prepare(out, variant, Path(stock_path).parent)
sources = [here/"root.S", app/"root.c", app/"events.c", app/"storage_ipc.c", app/"ui.c", app/"watchdog.c",
           app/("fixture.c" if variant=="qemu" else "spi_mmio.c"), here/"strings.c"]
root_objects = []
for source in sources:
    obj = source.name+".o"
    run(common + incs + defines + ["-I",out,"-std=gnu11","-c",source,"-o",obj])
    root_objects.append(obj)
rustc = os.environ.get("RUSTC") or shutil.which("rustc")
if not rustc:
    local = repo/"build/spike1/rustup/toolchains/1.91.1-aarch64-apple-darwin/bin/rustc"
    if not local.exists():
        sys.exit("Install Rust and run rustup target add armv7a-none-eabi")
    rustc = local
os.environ["VIOLET_FIRMWARE_RS"] = str(out/"firmware.rs")
run([rustc, "--edition=2021", "--target=armv7a-none-eabi", "--crate-type=staticlib", *(["--cfg","violet_qemu"] if variant=="qemu" else []),
     "-Cpanic=abort", "-Copt-level=s", app.parent/"services/lib.rs", "-o", "services.a"])
root_objects.append("services.a")
(out/"root.ld").write_text("ENTRY(_start)\nSECTIONS { . = 0x100000; .image : { *(.entry) *(.text*) *(.rodata*) . = ALIGN(4096); writable_start = .; *(.data*) *(.bss*) image_end = .; } /DISCARD/ : { *(.ARM.exidx*) } }")
run([ld,"--gc-sections","-T","root.ld",*root_objects,"-o","root.elf"])
run([objcopy,"-O","binary","root.elf","root.bin"])
assert (out/"root.bin").stat().st_size < 0x3e000
elf = (abi/"kernel.elf").read_bytes()
assert elf[:6] == b"\x7fELF\x01\x01" and struct.unpack_from("<I",elf,24)[0] == 0xe0000000
phoff, = struct.unpack_from("<I",elf,28)
size,count = struct.unpack_from("<HH",elf,42)
image = bytearray()
for i in range(count):
    kind,offset,va,pa,filesz,memsz,flags,align = struct.unpack_from("<8I",elf,phoff+i*size)
    if kind != 1: continue
    assert va-pa == 0xa0000000 and 0x40000000 <= pa <= pa+memsz <= 0x40100000 and filesz <= memsz
    index = pa-0x40000000
    if len(image)<index+memsz: image.extend(bytes(index+memsz-len(image)))
    image[index:index+filesz]=elf[offset:offset+filesz]
(out/"kernel.bin").write_bytes(image+bytes(-len(image)%4))
(out/"payload.S").write_text('.section .payload,"a"\n.global kernel,kernel_end,root,root_end\nkernel: .incbin "kernel.bin"\nkernel_end:\nroot: .incbin "root.bin"\nroot_end:\n')
fdt = build/"dtc-1.7.2/libfdt"
ndk = cc.parent.parent/"sysroot/usr/include"
headers = ["-I"+str(fdt),"-isystem",ndk/"arm-linux-androideabi","-isystem",ndk]
objects = []
for source in [here/"loader.S", here/"loader.c", here/"strings.c", out/"payload.S"]+[fdt/(n+".c") for n in ("fdt","fdt_ro","fdt_check","fdt_addresses")]:
    obj = source.name+".o"
    run(common+defines+headers+["-c",source,"-o",obj])
    objects.append(obj)
run([ld,"--gc-sections","-T",here/"loader.ld",*objects,"-o","loader.elf"])
run([objcopy,"-O","binary","loader.elf","zImage"])
z=(out/"zImage").read_bytes()
assert struct.unpack_from("<3I",z,36)==(0x016f2818,0,len(z))
if variant=="qemu":
    run(common+["-DIMAGE_BYTES="+str(len(z)),"-c",here/"qemu-entry.S","-o","qemu-entry.o"])
    run([objcopy,"-O","binary","-j",".text","qemu-entry.o","entry.bin"])
if variant=="moto":
    sys.path.insert(0,str(repo/"tools"))
    from phone_image import rebuild_v2
    stock=Path(stock_path).read_bytes()
    assert hashlib.sha256(stock).hexdigest()=="36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a"
    image=rebuild_v2(stock,kernel=z)
    (out/"boot.img").write_bytes(image)
    page, = struct.unpack_from("<I", stock, 36)
    assert image[page:page+len(z)]==z
    print(hashlib.sha256(image).hexdigest(),out/"boot.img")
