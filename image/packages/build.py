"""Link each guest component at its private virtual base, with a versioned package header."""

import binascii
import os
import json
from pathlib import Path
import re
import struct
import subprocess


def companion_key(repo, dest):
    """The iPhone companion's Shield-signing public key, pinned into the image.

    On a real pairing this is the Secure Enclave key's public half. For the
    emulator the Mac makes a P-256 key once (build/shield/companion-signing.pem)
    and hands the private half to the companion in the iOS Simulator, which has
    no Secure Enclave. Only the public key enters the image.
    """
    key = Path(os.environ.get("VIOLET_SHIELD_KEY", repo / "build/shield/companion-signing.pem"))
    if not key.exists():
        key.parent.mkdir(parents=True, exist_ok=True)
        tmp = key.with_suffix(".new")
        subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", tmp], check=True)
        tmp.chmod(0o600)
        tmp.replace(key)
    public = subprocess.check_output(["openssl", "ec", "-in", key, "-pubout", "-outform", "DER",
                                      "-conv_form", "uncompressed"], stderr=subprocess.DEVNULL)
    prefix = bytes.fromhex("3059301306072a8648ce3d020106082a8648ce3d030107034200")
    if len(public) != len(prefix) + 65 or not public.startswith(prefix + b"\x04"):
        raise SystemExit(f"{key} is not a P-256 key")
    out = dest / "companion_key.rs"
    out.write_text("pub const COMPANION_KEY: [u8; 65] = [" + ",".join(map(str, public[len(prefix):])) + "];\n")
    return out


def sentinel_key_path(repo, environ=None):
    """Where the handset Sentinel's build secret is kept (QEMU uses none).

    VIOLET_SENTINEL_SEAL names the file. A release (VIOLET_RELEASE=1) must name
    it: the build fails rather than invent one. A developer build keeps it
    beside the developer signing key when VIOLET_DEVELOPER_KEY_DIR is set
    (image/packages/sign.py), so every worktree shares it, else in this tree.
    """
    environ = os.environ if environ is None else environ
    if environ.get("VIOLET_SENTINEL_SEAL"):
        return Path(environ["VIOLET_SENTINEL_SEAL"])
    if environ.get("VIOLET_RELEASE") == "1":
        raise SystemExit("VIOLET_RELEASE=1 needs VIOLET_SENTINEL_SEAL: the Sentinel's sealing secret (system/sentinel/README.md)")
    if environ.get("VIOLET_DEVELOPER_KEY_DIR"):
        return Path(environ["VIOLET_DEVELOPER_KEY_DIR"]) / "sentinel-seal.bin"
    return repo / "build/sentinel/seal-secret.bin"


def sentinel_key(repo, dest):
    """The handset Sentinel's sealing secret (system/sentinel/seal.rs), built into its package only.

    32 random bytes, made once where `sentinel_key_path` says. Phones built
    from the same file share it, and a Sentinel built with another secret
    opens nothing the old one sealed. A QEMU Sentinel has no build secret: it
    derives its key from the device root key (cfg device_root_key).
    """
    key = sentinel_key_path(repo)
    if not key.exists() and os.environ.get("VIOLET_SENTINEL_SEAL"):
        raise SystemExit(f"VIOLET_SENTINEL_SEAL: {key} not found")
    if not key.exists():
        key.parent.mkdir(parents=True, exist_ok=True)
        tmp = key.with_suffix(".new")
        tmp.write_bytes(os.urandom(32))
        tmp.chmod(0o600)
        tmp.replace(key)
    secret = key.read_bytes()
    if len(secret) != 32:
        raise SystemExit(f"{key} is not a 32-byte sealing secret")
    out = dest / "sentinel_key.rs"
    out.write_text("pub const SEAL_SECRET: [u8; 32] = [" + ",".join(map(str, secret)) + "];\n")
    return out


BASE = 0x100000


def component_script(arch):
    """Every component's linker script: linked at BASE, its entry first.

    AArch64 also drops the unwind tables clang and rustc emit there (panics
    abort; nothing unwinds). The AArch32 text is the one the build always used.
    """
    discard = "*(.ARM.exidx*) *(.ARM.extab*)" + (" *(.eh_frame*)" if arch == "aarch64" else "")
    return f"""ENTRY(process_entry)
SECTIONS {{
 . = {BASE:#x};
 .text : {{ KEEP(*(.entry)) *(.text*) *(.rodata*) . = ALIGN(4096); }}
 .data : {{ *(.data*) *(.got*) }}
 .bss (NOLOAD) : {{ *(.bss*) *(COMMON) }}
 image_end = .;
 /DISCARD/ : {{ {discard} }}
}}"""


def link_flags(arch):
    """ld.lld flags beyond the script. lld's AArch64 default page size is 64 KiB;
    the supervisor maps packages in 4 KiB pages on both architectures."""
    return ["-z", "max-page-size=4096"] if arch == "aarch64" else []


def package_image(data, skip_bss_at=None):
    """(code size, image bytes) of a linked component ELF (ELF32 or ELF64).

    The image is every allocated section from BASE, NOBITS left zero, except a
    NOBITS section at `skip_bss_at` (the handset Radio's private RAM, which is
    outside its image). The code (.text) must start at BASE and fill whole pages.
    """
    import violet_arch
    sections, idx = violet_arch.sections(data)
    st = sections[idx]
    strings = data[st[4] : st[4] + st[5]]
    allocated = [s for s in sections if s[2] & 2 and s[5]]
    code = next(s for s in allocated if strings[s[0] :].split(b"\0")[0] == b".text")
    if code[3] != BASE or code[5] % 4096:
        raise ValueError(f".text must start at {BASE:#x} and fill whole pages")
    if skip_bss_at is not None:
        allocated = [s for s in allocated if not (s[1] == 8 and s[3] == skip_bss_at)]
    end = max(s[3] + s[5] for s in allocated)
    image = bytearray(end - BASE)
    for s in allocated:
        if s[1] != 8:
            image[s[3] - BASE : s[3] - BASE + s[5]] = data[s[4] : s[4] + s[5]]
    return code[5], image


def components(repo, out, cc, common, incs, rustc, objects, only=None, selected=None, storage_backend=None, target="qemu"):
    from package_format import package_abi, radio_abi
    # The architecture comes from VIOLET_ARCH (violet_arch.py): aarch32 unless set.
    # It decides the Rust target, the link, and the architecture recorded in
    # every package's ABI word (package_format.ARCH_ABI); `common` and `incs`
    # (the C flags and libsel4 headers) come from the caller for the same one.
    import violet_arch
    violet_arch.require_components()
    arch = violet_arch.selected()

    platform_include = repo / "platform/sel4/include"
    dest = out / "packages"
    dest.mkdir(exist_ok=True)

    def run(args):
        subprocess.run(args, check=True, cwd=out)

    glyphs = (repo / "src/glyphs.h").read_text()
    alphabet = re.search(r'alphabet\[\] = "([^"]+)"', glyphs)[1]
    rows = re.findall(r"\{([\d, ]+)\}", glyphs)
    (out / "glyphs.rs").write_text(
        'const ALPHABET:&[u8]=b"'
        + alphabet
        + '";\nconst GLYPHS:&[[u8;5]]=&['
        + ",".join("[" + row + "]" for row in rows)
        + "];"
    )
    import os

    os.environ["VIOLET_GLYPHS_RS"] = str(out / "glyphs.rs")
    scripts = {
        "launcher": repo / "apps/launcher/process.rs",
        "runtime": repo / "runtime/process.rs",
        "display": repo / "services/display/process.rs",
        "radio": repo / "services/radio/process.rs",
        "power": repo / "services/power/process.rs",
        "audio": repo / "services/audio/process.rs",
        "camera": repo / "services/camera/process.rs",
        "open": repo / "runtime/open/mod.rs",
        "sentinel": repo / "system/sentinel/process.rs",
    }
    board = repo / "targets" / ("moto-g-pure" if target == "moto" else "qemu")
    if target == "moto":
        scripts["radio"] = board / "radio.rs"
        scripts["usb-diagnostics"] = repo / "services/usb-diagnostics/process.rs"
    script = dest / "component.ld"
    script.write_text(component_script(arch))
    transport = dest / "transport.o"
    run(
        common
        + incs
        + ["-DNDEBUG", "-I", platform_include, "-c", repo / "runtime/transport.c", "-o", transport]
    )
    manifest = json.loads((repo / "image/catalog/components.json").read_text())
    from sign import config as signing_config

    package_signer = signing_config(repo, target)
    from sign import version_overrides
    test_versions = version_overrides(package_signer)
    unknown = set(test_versions) - {item.get("name") for item in manifest}
    if unknown:
        raise SystemExit(f"VIOLET_PACKAGE_VERSIONS names no component: {sorted(unknown)}")
    # Built-in library apps (IDs 11–14) name their source; they ship with the APP1 trailer.
    for item in manifest:
        if "entry" in item:
            item["entry"] = repo / item["entry"]
            scripts[item["name"]] = item["entry"]
    if target == "moto":
        manifest.append({"id": 21, "name": "usb-diagnostics", "required": True})
    if selected is not None:
        manifest = [item for item in manifest if item["name"] in selected]
    names = [item["name"] for item in manifest]
    external = os.environ.get("VIOLET_APP_DIR") if selected is None else None
    if external:
        folder = Path(external).resolve()
        entry = json.loads((folder / "app.json").read_text())
        if not 11 <= entry["id"] <= 14 or not re.fullmatch(
            r"[A-Z0-9 -]{1,16}", entry["title"]
        ):
            raise ValueError(
                "app ID must be 11–14 and title must fit 16 display characters"
            )
        name = f"app-{entry['id']}"
        entry["name"] = name
        entry["entry"] = folder / "process.rs"
        scripts[name] = entry["entry"]
        manifest.append(entry)
        names.append(name)
        only = name
    if only is not None and only not in [*names, "package_manager"]:
        raise ValueError(f"Unknown component: {only}")
    # Open apps' wall clock starts at the source's commit time (no RTC driver yet).
    if "VIOLET_BUILD_EPOCH" not in os.environ:
        stamp = subprocess.run(["git", "log", "-1", "--format=%ct"], cwd=repo, capture_output=True, text=True).stdout.strip()
        os.environ["VIOLET_BUILD_EPOCH"] = os.environ.get("SOURCE_DATE_EPOCH", stamp or "1795000000")
    # Package manager is always loaded from the factory recovery image.
    manifest.append({"id": 9, "name": "package_manager"})
    scripts["package_manager"] = repo / "system/package-manager/process.rs"
    for item in manifest:
        name = item["name"]
        if only and name != only:
            continue
        modules = {"protocol": repo / "runtime/protocol.rs"}
        if name == "power":
            modules["power_backend"] = board / "power.rs"
        if name == "audio":
            modules["audio_backend"] = board / "audio.rs"
        if name == "camera":
            modules["camera_backend"] = board / "camera.rs"
            if target == "moto":
                import sys

                sys.path.insert(0, str(repo / "tools"))
                from sensor_tables import generate

                generate(dest / "sensor_tables.rs", repo / "build/downloads")
                modules["sensor_tables"] = dest / "sensor_tables.rs"
        if name in scripts:
            modules["process"] = scripts[name]
        if name == "launcher" or "entry" in item:
            modules.update(
                sdk=repo / "runtime/sdk.rs", drawing=repo / "apps/drawing.rs"
            )
        if name == "open":
            modules["companion_key"] = companion_key(repo, dest)
        if name == "sentinel" and target == "moto":
            modules["sentinel_key"] = sentinel_key(repo, dest)
        body = "process::run()"
        if name == "runtime":
            modules["lanes"] = repo / "runtime/lanes.rs"
            body = "if identity>=15 {lanes::work(identity-15+4)} else {process::run()}"
        if name == "touch":
            modules.update(
                touch=repo / "services/touch/touch.rs",
                spi=repo / "services/touch/spi.rs",
            )
            body = 'unsafe { extern "C" {fn touch_task();} touch_task(); }'
        if name == "storage":
            modules.update(
                storage=repo / "services/storage/storage.rs",
                block=storage_backend or repo / "services/storage/block.rs",
                journal=repo / "services/storage/journal.rs",
                # The trait VioletFS (and the cipher under it) sit on: /data.
                blockdev=repo / "services/storage/blockdev.rs",
            )
            body = "storage::storage_task()"
        crate = dest / (name + ".rs")
        # The Open Runtime keeps its tables on a heap.
        prelude = "extern crate alloc;\n" if name in ("open", "sentinel") else ""
        crate.write_text(
            "#![no_std]\n#![allow(dead_code)]\n" + prelude
            + "".join(
                f"#[path={json.dumps(str(v))}] mod {k};\n" for k, v in modules.items()
            )
            + '#[panic_handler] fn panic(_: &core::panic::PanicInfo)->!{unsafe{core::arch::asm!("udf #1",options(noreturn))}}\n'
            + '#[no_mangle] pub extern "C" fn component_main(identity:u32){let _=identity;'
            + body
            + "}\n"
        )
        lib = dest / (name + ".a")
        run(
            [
                rustc,
                "--edition=2021",
                "--crate-type=staticlib",
                "--target=" + violet_arch.rust_target(),
                "-Cpanic=abort",
                # The Open Runtime (with the Sentinel) is optimised for size; its window is 0x80000 bytes.
                "-Copt-level=z" if name in ("open", "sentinel") else "-Copt-level=s",
                # The Open Runtime is also optimised together with core and
                # alloc (fat LTO): their unused generic code goes, which keeps
                # the handset catalog inside its 1 MiB banks.
                *(["-Clto=fat"] if name in ("open", "sentinel") else []),
                # One codegen unit: smaller code.
                "-Ccodegen-units=1",
                *(["--cfg", "moto_board"] if target=="moto" and name in ("power", "open", "launcher") else []),
                # Test images only: the Open Runtime damages its own requests about
                # paths ending in "sentinel-fault-probe" (runtime/open/policy.rs).
                *(["--cfg", "sentinel_fault"] if name == "open" and os.environ.get("VIOLET_SENTINEL_FAULT") == "1" else []),
                # QEMU: the Sentinel's sealing key comes from the device root key page.
                *(["--cfg", "device_root_key"] if name == "sentinel" and target != "moto" else []),
                # Test images only: a system call that makes the Open Runtime fault, to
                # show the fault ends one app (runtime/open/contain.rs). Never in a release.
                *(["--cfg", "open_fault_probe"] if name == "open" and os.environ.get("VIOLET_TEST_FAULT") == "1"
                  and os.environ.get("VIOLET_RELEASE") != "1" else []),
                *(["--cfg", "boot_trace", "--cfg", "package_catalog"] if target == "moto" and name == "storage" else []),
                # The AArch64 handset marks its running boot slot successful
                # (targets/moto-g-pure/emmc.rs mark_running_slot): only misc's
                # bootloader_control record, after the supervisor's health signal.
                *(["--cfg", "slot_mark"] if target == "moto" and name == "storage" and arch == "aarch64" else []),
                crate,
                "-o",
                lib,
            ]
        )
        tls_objects = []
        if name == "open":
            shim = dest / "open-sel4.o"
            run([*common, *incs, "-DNDEBUG", "-std=gnu11", "-I", platform_include, "-c", repo / "runtime/open/sel4.c", "-o", shim])
            tls_objects.append(shim)
            # The Sentinel verifies Shields with BearSSL's P-256, like the supervisor.
            import sys

            sys.path.insert(0, str(repo / "services/radio"))
            from build_tls import build as build_crypto

            bearssl = build_crypto(repo, out, common)[1]
            crypto = dest / "sentinel-crypto.o"
            run([*common, "-I", repo / "build/sel4/bearssl-0.6/inc", "-I", repo / "services/radio/freestanding",
                 "-c", repo / "services/sentinel/crypto.c", "-o", crypto])
            tls_objects += [crypto, bearssl]
        if name == "radio" and target == "qemu":
            import sys

            sys.path.insert(0, str(repo / "services/radio"))
            from build_tls import build

            tls_objects = build(repo, out, common)
            platform_tls = dest / "tls-platform.o"
            run([*common, "-I", repo / "services/radio/freestanding", "-c",
                 board / "tls_platform.c", "-o", platform_tls])
            tls_objects.append(platform_tls)
        if name == "radio" and target == "moto":
            import sys
            sys.path.insert(0, str(board))
            from build_supplicant import objects as supplicant_objects
            from build_ip import objects as ip_objects
            sys.path.insert(0, str(repo / "services/radio"))
            from build_tls import build
            tls_objects.extend(supplicant_objects(repo, out, common))
            tls_objects.extend(ip_objects(repo, out, common))
            tls_objects.extend(build(repo, out, common))
        moto_radio = target == "moto" and name == "radio"
        link_script = script
        if moto_radio:
            # ABI 4 has 192 KiB of zeroed private RAM outside the package/stack
            # (ABI 3 had 128 KiB; 32 socket slots' lwIP tables no longer fit it).
            # A supervisor from before ABI 4 refuses the package instead of
            # letting Radio fault past the memory it mapped.
            link_script = dest / "radio.ld"
            link_script.write_text(script.read_text().replace(".bss (NOLOAD)", ". = 0x02800000; .bss (NOLOAD)")
                                   + "\nASSERT(SIZEOF(.bss) <= 0x30000, \"Radio private RAM exceeded\")\n")
        elf = dest / (name + ".elf")
        run(
            [
                cc.with_name("ld.lld"),
                "--gc-sections",
                *link_flags(arch),
                "-T",
                link_script,
                "--start-group",
                transport,
                *objects,
                *tls_objects,
                lib,
                "--end-group",
                "-o",
                elf,
            ]
        )
        code_size, image = package_image(elf.read_bytes(), 0x02800000 if moto_radio else None)
        if "entry" in item:
            image += b"APP1" + item["title"].encode("ascii").ljust(16, b"\0")
            if len(image) > 65512:
                raise ValueError("application exceeds 64 KiB package slot")
        # The supervisor rejects a package over its image window and boots
        # whatever older copy it has, so stop here instead.
        from package_format import package_window
        window = package_window(item["id"])
        if len(image) > window:
            raise SystemExit(f"{name}: {len(image):#x} bytes, over its {window:#x}-byte package window")
        header = struct.pack(
            "<4s5I",
            b"VPK2",
            radio_abi(4, arch) if moto_radio else package_abi(item["id"], arch),
            item["id"],
            code_size,
            len(image),
            binascii.crc32(image),
        )
        # Every package carries a publisher signature over its header, image and
        # security version (sign.py); the supervisor runs nothing without one.
        (dest / (name + ".vpk")).write_bytes(package_signer.sign(header + image, int(test_versions.get(name, item.get("version", 1)))))
    if only:
        return
    from package_format import catalog_limit, pack

    (out / "catalog.bin").write_bytes(
        pack([(dest / (n + ".vpk")).read_bytes() for n in names], limit=catalog_limit(target), arch=arch)
    )
