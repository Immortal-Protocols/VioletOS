"""Link the VioletOS first task and package it with the target kernel."""
from pathlib import Path
import sys, subprocess, struct, shlex, hashlib, os, shutil

here = Path(__file__).resolve().parent
repo = here.parents[2]
build = repo / "build/sel4"
variant, stock_path = sys.argv[1:]
platform_include = here.parent / "include"
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
# Cores the kernel boots (kernel/sel4/build.py); the loader starts the others.
# The root task's window: its image (the supervisor and, on the handset, the
# built-in catalog) with its stack above. 1.5 MiB at 0x40200000 physical; the
# loader proves 0x40000000-0x44000000 holds no firmware reservation.
ROOT_BYTES = 0x180000
defines = ["-DNDEBUG", f"-DROOT_BYTES={ROOT_BYTES:#x}", "-DVIOLET_CORES=" + os.environ.get("VIOLET_CORES", "1")]
if probe: defines.append("-DVIOLET_TEST_ISOLATION")
defines.append("-DVIOLET_QEMU" if variant=="qemu" else "-DVIOLET_MOTO")
sys.path.insert(0,str(repo / "services/touch"))
from firmware import prepare
prepare(out, variant, Path(stock_path).parent)
sources = [here/"root.S", here/"strings.c"]
for source in sources:
    obj = source.name+".o"
    run(common + incs + defines + ["-I",out,"-I",platform_include,"-std=gnu11","-c",source,"-o",obj])
rustc = os.environ.get("RUSTC") or shutil.which("rustc")
if not rustc:
    local = repo/"build/spike1/rustup/toolchains/1.91.1-aarch64-apple-darwin/bin/rustc"
    if not local.exists():
        sys.exit("Install Rust and run rustup target add armv7a-none-eabi")
    rustc = local
os.environ["VIOLET_FIRMWARE_RS"] = str(out/"firmware.rs")
sys.path.insert(0,str(repo/"image/packages"))
os.environ["VIOLET_CATALOG_VOLUME_RS"] = str(repo/"system/package-manager/catalog_volume.rs")
from build import components
# Only process-local ABI shims, never the root allocator, enter packages.
board_spi = repo/("targets/qemu/fixture.c" if variant=="qemu" else "targets/moto-g-pure/spi_mmio.c")
shims=[]
for source in [repo/"services/touch/events.c", repo/"services/storage/storage_ipc.c", here.parent/"board/watchdog.c", board_spi, here/"strings.c"]:
    obj=out/("component-"+source.name+".o")
    run(common+incs+defines+["-DVIOLET_COMPONENT","-I",out,"-I",platform_include,"-std=gnu11","-c",source,"-o",obj])
    shims.append(obj)
only=os.environ.get("VIOLET_COMPONENT_ONLY")
selected=None if variant=="qemu" else {"launcher","runtime","touch","display","storage","power","radio","usb-diagnostics","audio","open","camera","sentinel"}
storage_backend=repo/"targets/moto-g-pure/emmc.rs" if variant=="moto" else None
components(repo,out,cc,common,incs,rustc,shims,only,selected,storage_backend,variant)
if only or (variant=="qemu" and os.environ.get("VIOLET_APP_DIR")): sys.exit(0)
(out/"catalog.S").write_text('.section .rodata.catalog,"a"\n.balign 4\n.global builtin_catalog,builtin_catalog_end\nbuiltin_catalog: .incbin "catalog.bin"\nbuiltin_catalog_end:\n.balign 4\n.global recovery_manager\nrecovery_manager: .incbin "packages/package_manager.vpk"\n')
run(common+["-c",out/"catalog.S","-o","catalog.o"])
# Package signatures. The supervisor verifies every VPK2 (system/package-manager/
# verify.rs, with BearSSL's SHA-256 and P-256) against the keys pinned here:
# the per-build developer key, or only the release root when VIOLET_RELEASE=1
# (image/packages/sign.py). Only public keys enter the image.
from sign import config as signing_config, write_keys, summary as sign_summary, DEVELOPER_KEY_ID
package_signer = signing_config(repo, variant)
os.environ["VIOLET_PACKAGE_KEYS_RS"] = str(write_keys(package_signer, out))
sys.path.insert(0, str(repo / "services/radio"))
from build_tls import build as build_crypto
crypto_incs = ["-I", build / "bearssl-0.6/inc", "-I", repo / "services/radio/freestanding"]
run(common + crypto_incs + ["-c", repo/"system/package-manager/verify_crypto.c", "-o", "verify-crypto.o"])
# An object, not a second staticlib: core comes from the root's one Rust archive.
run([rustc, "--edition=2021", "--target=armv7a-none-eabi", "--crate-type=lib", "--emit=obj", "-Cpanic=abort",
     "-Copt-level=s", "-Ccodegen-units=1", repo/"system/package-manager/verify.rs", "-o", "package-verify.o"])
signing_objects = ["package-verify.o", "verify-crypto.o", build_crypto(repo, out, common)[1]]
if variant == "moto" and DEVELOPER_KEY_ID in package_signer.keys:
    # Developer images also accept a detached signature by the developer key to
    # activate a Radio or USB Diagnostics package over USB without a tap. A
    # release image has no developer key, so it has no such path.
    (out / "developer-key.h").write_text("static unsigned char developer_key[] = {" +
        ",".join(map(str, package_signer.keys[DEVELOPER_KEY_ID])) + "};\n")
    defines.append("-DVIOLET_DEVELOPER_KEY")
run(common+incs+defines+["-I",out,"-I",platform_include,"-std=gnu11","-c",repo/"system/supervisor/supervisor.c","-o","supervisor.o"])
root_objects=["root.S.o","supervisor.o","strings.c.o","catalog.o"]
if variant=="qemu":
    run([rustc,"--edition=2021","--target=armv7a-none-eabi","--crate-type=staticlib","--cfg","package_volume","-Cpanic=abort","-Copt-level=s",repo/"system/package-manager/volume.rs","-o","volume.a"])
    root_objects.insert(2,"volume.a")
    root_objects.extend(signing_objects)
else:
    run(common+incs+defines+["-I",platform_include,"-c",board_spi,"-o","board-map.o"])
    root_objects.append("board-map.o")
    for name, flags in [("loader-history.a", ["--cfg", "loader_trace", "--cfg", "package_catalog"]), ("root-history.a", ["--cfg", "package_catalog"])]:
        run([rustc, "--edition=2021", "--target=armv7a-none-eabi", "--crate-type=staticlib",
             "--cfg", "boot_trace", *flags, "-Cpanic=abort", "-Copt-level=s",
             repo/"targets/moto-g-pure/boot_trace.rs", "-o", name])
    root_objects.extend(["root-history.a", *signing_objects])
(out/"root.ld").write_text("ENTRY(_start)\nSECTIONS { . = 0x100000; .image : { *(.entry) *(.text*) *(.rodata*) . = ALIGN(4096); writable_start = .; *(.data*) *(.bss*) image_end = .; } /DISCARD/ : { *(.ARM.exidx*) } }")
run([ld,"--gc-sections","-T","root.ld",*root_objects,"-o","root.elf"])
run([objcopy,"-O","binary","root.elf","root.bin"])
# The root task's stack is what is left of its window (root.S), and
# package signatures are checked on it (SHA-256 and P-256: a few KiB).
root_bytes = (out/"root.bin").stat().st_size
print(f"root.bin: {root_bytes} bytes, {ROOT_BYTES - root_bytes} left for the root stack (at least 8192 required)")
if root_bytes > ROOT_BYTES - 8192:
    sys.exit(f"root.bin is {root_bytes} bytes: under 8192 bytes would be left for the root task's stack")
# Say which keys this image trusts: a developer image must never pass for a release.
print(sign_summary(package_signer))
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
# Open apps' file system image: QEMU loads it at 0x4a000000, the Moto loader carries it.
sys.path.insert(0,str(repo/"tools"))
from build_initfs import main as build_initfs
# The handset carries it gzip-compressed as its boot image's ramdisk.
from build_initfs import HANDSET_SKIP
build_initfs(out/"initfs.cpio", compress=variant=="moto", **(dict(python=False, skip=HANDSET_SKIP) if variant=="moto" else {}))
if variant == "moto":
    sys.path.insert(0, str(repo/"targets/moto-g-pure"))
    from wifi_firmware import prepare_wifi
    prepare_wifi(Path(stock_path).parent, out/"wifi-firmware.bin")
    with (out/"payload.S").open("a") as payload:
        payload.write('.balign 4\n.global radio_firmware,radio_firmware_end\nradio_firmware: .incbin "wifi-firmware.bin"\nradio_firmware_end:\n')
verified_boot = []
if variant == "qemu":
    # Verified boot (boot/loader/verified.rs, docs/verified-boot.md): the loader
    # starts the kernel and the root task only if they match a manifest signed
    # by a key this image pins. The /apps image is bound too in a release, or
    # with VIOLET_BOOT_BIND_APPS=1; a developer image leaves it unbound because
    # the local tests swap it without relinking the loader. The handset loader
    # carries no manifest: its bootloader is unlocked and verifies nothing.
    from boot_manifest import build as boot_manifest, verify as check_boot_manifest, system_version, check_loader
    bind_apps = package_signer.release or os.environ.get("VIOLET_BOOT_BIND_APPS") == "1"
    boot_images = [(out/"kernel.bin").read_bytes(), (out/"root.bin").read_bytes(),
                   (out/"initfs.cpio").read_bytes() if bind_apps else None]
    # The signed version is the root task's own (its VIOLETSYSVER note), never a re-read of the environment.
    try:
        boot_version = system_version(boot_images[1], os.environ.get("VIOLET_SYSTEM_VERSION"), package_signer.release)
    except ValueError as e:
        sys.exit(f"verified boot: {e}")
    signed_manifest = boot_manifest(package_signer, boot_version, *boot_images)
    check_boot_manifest(signed_manifest, package_signer.keys, variant, *boot_images, require_apps=package_signer.release)
    (out/"boot-manifest.bin").write_bytes(signed_manifest)
    with (out/"payload.S").open("a") as payload:
        payload.write('.balign 4\n.global boot_manifest,boot_manifest_end\nboot_manifest: .incbin "boot-manifest.bin"\nboot_manifest_end:\n')
    run([rustc, "--edition=2021", "--target=armv7a-none-eabi", "--crate-type=staticlib", "-Cpanic=abort",
         "-Copt-level=s", "-Ccodegen-units=1", repo/"boot/loader/verified_lib.rs", "-o", "loader-verified.a"])
    defines.append("-DVIOLET_VERIFIED_BOOT")
    verified_boot = ["loader-verified.a", *signing_objects[1:]]
    print("Verified boot: the loader checks the kernel and the root task" + (" and the /apps image." if bind_apps else "; the /apps image is NOT bound (developer image)."))
fdt = build/"dtc-1.7.2/libfdt"
ndk = cc.parent.parent/"sysroot/usr/include"
headers = ["-I"+str(fdt),"-isystem",ndk/"arm-linux-androideabi","-isystem",ndk]
objects = []
for source in [repo/"boot/loader/loader.S", repo/"boot/loader/loader.c", here/"strings.c", out/"payload.S"]+[fdt/(n+".c") for n in ("fdt","fdt_ro","fdt_check","fdt_addresses")]:
    obj = source.name+".o"
    run(common+defines+headers+["-c",source,"-o",obj])
    objects.append(obj)
if variant == "moto": objects.append("loader-history.a")
objects.extend(verified_boot)
run([ld,"--gc-sections","-T",repo/"boot/loader/loader.ld",*objects,"-o","loader.elf"])
run([objcopy,"-O","binary","loader.elf","zImage"])
z=(out/"zImage").read_bytes()
assert struct.unpack_from("<3I",z,36)==(0x016f2818,0,len(z))
if variant=="qemu":
    # A QEMU loader that dropped the check (no VIOLET_VERIFIED_BOOT, or no call) fails the build.
    try:
        check_loader(z, signed_manifest)
    except ValueError as e:
        sys.exit(f"verified boot: {e}")
    run(common+["-DIMAGE_BYTES="+str(len(z)),"-c",here/"qemu-entry.S","-o","qemu-entry.o"])
    run([objcopy,"-O","binary","-j",".text","qemu-entry.o","entry.bin"])
if variant=="moto":
    sys.path.insert(0,str(repo/"tools"))
    from phone_image import rebuild_v2
    stock=Path(stock_path).read_bytes()
    assert hashlib.sha256(stock).hexdigest()=="36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a"
    # The bootloader loads the ramdisk; VioletOS's loader copies it to the open apps' window.
    image=rebuild_v2(stock,(out/"initfs.cpio").read_bytes(),kernel=z)
    # The storage marker, volume, boot history and catalog banks (1.25 MiB of
    # catalog each) after it, as the AArch64 build lays them out too.
    from phone_image import handset_storage
    import package_format
    image=handset_storage(image, package_format.HANDSET_LIMIT)
    (out/"boot.img").write_bytes(image)
    page, = struct.unpack_from("<I", stock, 36)
    assert image[page:page+len(z)]==z
    print(hashlib.sha256(image).hexdigest(),out/"boot.img")
