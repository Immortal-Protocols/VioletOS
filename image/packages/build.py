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


def components(repo, out, cc, common, incs, rustc, objects, only=None, selected=None, storage_backend=None, target="qemu"):
    from package_format import package_abi

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
        "wifi": repo / "apps/wifi/process.rs",
        "stopwatch": repo / "apps/stopwatch/process.rs",
        "runtime": repo / "runtime/process.rs",
        "display": repo / "services/display/process.rs",
        "radio": repo / "services/radio/process.rs",
        "power": repo / "services/power/process.rs",
        "audio": repo / "services/audio/process.rs",
        "open": repo / "runtime/open/mod.rs",
    }
    board = repo / "targets" / ("moto-g-pure" if target == "moto" else "qemu")
    if target == "moto":
        scripts["radio"] = board / "radio.rs"
        scripts["usb-diagnostics"] = repo / "services/usb-diagnostics/process.rs"
    script = dest / "component.ld"
    script.write_text("""ENTRY(process_entry)
SECTIONS {
 . = 0x100000;
 .text : { KEEP(*(.entry)) *(.text*) *(.rodata*) . = ALIGN(4096); }
 .data : { *(.data*) *(.got*) }
 .bss (NOLOAD) : { *(.bss*) *(COMMON) }
 image_end = .;
 /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) }
}""")
    transport = dest / "transport.o"
    run(
        common
        + incs
        + ["-DNDEBUG", "-I", platform_include, "-c", repo / "runtime/transport.c", "-o", transport]
    )
    manifest = json.loads((repo / "image/catalog/components.json").read_text())
    titles = {item["name"]: item.get("title") for item in manifest}
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
    os.environ["VIOLET_WATCH_TITLE"] = titles["stopwatch"]
    os.environ["VIOLET_WIFI_TITLE"] = titles["wifi"]
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
        if name in scripts:
            modules["process"] = scripts[name]
        if name in ("stopwatch", "launcher", "wifi") or "entry" in item:
            modules.update(
                sdk=repo / "runtime/sdk.rs", drawing=repo / "apps/drawing.rs"
            )
        if name == "stopwatch":
            modules["watch"] = repo / "apps/stopwatch/state.rs"
        if name == "open":
            modules["companion_key"] = companion_key(repo, dest)
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
            )
            body = "storage::storage_task()"
        crate = dest / (name + ".rs")
        # The Open Runtime keeps its tables on a heap.
        prelude = "extern crate alloc;\n" if name == "open" else ""
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
                "--target=armv7a-none-eabi",
                "-Cpanic=abort",
                # The Open Runtime (with the Sentinel) is optimised for size to fit the 0x3e000-byte window.
                "-Copt-level=z" if name == "open" else "-Copt-level=s",
                # One codegen unit: smaller code, which also helps the Open Runtime
                # stay inside the 0x3E000-byte package window.
                "-Ccodegen-units=1",
                *(["--cfg", "moto_board"] if target=="moto" and name in ("power", "open", "launcher") else []),
                *(["--cfg", "boot_trace", "--cfg", "package_catalog"] if target == "moto" and name == "storage" else []),
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
            # ABI 3 has 128 KiB of zeroed private RAM outside the package/stack.
            link_script = dest / "radio.ld"
            link_script.write_text(script.read_text().replace(".bss (NOLOAD)", ". = 0x02800000; .bss (NOLOAD)")
                                   + "\nASSERT(SIZEOF(.bss) <= 0x20000, \"Radio private RAM exceeded\")\n")
        elf = dest / (name + ".elf")
        run(
            [
                cc.with_name("ld.lld"),
                "--gc-sections",
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
        data = elf.read_bytes()
        off = struct.unpack_from("<I", data, 32)[0]
        size, count, idx = struct.unpack_from("<3H", data, 46)
        sections = [
            struct.unpack_from("<10I", data, off + j * size) for j in range(count)
        ]
        st = sections[idx]
        strings = data[st[4] : st[4] + st[5]]
        allocated = [s for s in sections if s[2] & 2 and s[5]]
        code = next(s for s in allocated if strings[s[0] :].split(b"\0")[0] == b".text")
        assert code[3] == 0x100000 and code[5] % 4096 == 0
        if moto_radio:
            allocated = [s for s in allocated if not (s[1] == 8 and s[3] == 0x02800000)]
        end = max(s[3] + s[5] for s in allocated)
        image = bytearray(end - 0x100000)
        for s in allocated:
            if s[1] != 8:
                image[s[3] - 0x100000 : s[3] - 0x100000 + s[5]] = data[
                    s[4] : s[4] + s[5]
                ]
        if "entry" in item:
            image += b"APP1" + item["title"].encode("ascii").ljust(16, b"\0")
            if len(image) > 65512:
                raise ValueError("application exceeds 64 KiB package slot")
        # The supervisor rejects a package over its image window and boots
        # whatever older copy it has, so stop here instead.
        window = 0x38000 if item["id"] == 3 else 0x3E000
        if len(image) > window:
            raise SystemExit(f"{name}: {len(image):#x} bytes, over its {window:#x}-byte package window")
        header = struct.pack(
            "<4s5I",
            b"VPK1",
            3 if moto_radio else package_abi(item["id"]),
            item["id"],
            code[5],
            len(image),
            binascii.crc32(image),
        )
        (dest / (name + ".vpk")).write_bytes(header + image)
    if only:
        return
    from package_format import pack

    (out / "catalog.bin").write_bytes(
        pack([(dest / (n + ".vpk")).read_bytes() for n in names])
    )
