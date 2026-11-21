"""Link each guest component at its private virtual base, with a versioned package header."""

import binascii
import json
from pathlib import Path
import re
import struct
import subprocess


def components(repo, out, cc, common, incs, rustc, objects, only=None):
    app = repo / "platform/sel4/stopwatch"
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
    }
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
        + ["-DNDEBUG", "-I", app, "-c", repo / "runtime/transport.c", "-o", transport]
    )
    manifest = json.loads((repo / "image/catalog/components.json").read_text())
    names = [item["name"] for item in manifest]
    if only is not None and only not in [*names,"package_manager"]:
        raise ValueError(f"Unknown component: {only}")
    os.environ["VIOLET_WATCH_TITLE"] = next(
        item["title"] for item in manifest if item["name"] == "stopwatch"
    )
    os.environ["VIOLET_WIFI_TITLE"] = next(item["title"] for item in manifest if item["name"]=="wifi")
    # Package manager is always loaded from the factory recovery image.
    manifest.append({"id": 9, "name": "package_manager"})
    scripts["package_manager"] = repo / "system/package-manager/process.rs"
    for item in manifest:
        name = item["name"]
        if only and name != only:
            continue
        modules = {"protocol": repo / "runtime/protocol.rs"}
        if name in scripts:
            modules["process"] = scripts[name]
        if name in ("stopwatch", "launcher","wifi"):
            modules.update(
                sdk=repo / "runtime/sdk.rs", drawing=repo / "apps/drawing.rs"
            )
        if name == "stopwatch":
            modules["watch"] = repo / "apps/stopwatch/state.rs"
        body = "process::run()"
        if name == "touch":
            modules.update(
                touch=repo / "services/touch/touch.rs",
                spi=repo / "services/touch/spi.rs",
            )
            body = 'unsafe { extern "C" {fn touch_task();} touch_task(); }'
        if name == "storage":
            modules.update(
                storage=repo / "services/storage/storage.rs",
                block=repo / "services/storage/block.rs",
                journal=repo / "services/storage/journal.rs",
            )
            body = "storage::storage_task()"
        crate = dest / (name + ".rs")
        crate.write_text(
            "#![no_std]\n#![allow(dead_code)]\n"
            + "".join(
                f"#[path={json.dumps(str(v))}] mod {k};\n" for k, v in modules.items()
            )
            + '#[panic_handler] fn panic(_: &core::panic::PanicInfo)->!{unsafe{core::arch::asm!("udf #1",options(noreturn))}}\n'
            + '#[no_mangle] pub extern "C" fn component_main(){'
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
                "-Copt-level=s",
                crate,
                "-o",
                lib,
            ]
        )
        elf = dest / (name + ".elf")
        run(
            [
                cc.with_name("ld.lld"),
                "--gc-sections",
                "-T",
                script,
                "--start-group",
                transport,
                *objects,
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
        end = max(s[3] + s[5] for s in allocated)
        image = bytearray(end - 0x100000)
        for s in allocated:
            if s[1] != 8:
                image[s[3] - 0x100000 : s[3] - 0x100000 + s[5]] = data[
                    s[4] : s[4] + s[5]
                ]
        header = struct.pack(
            "<4s5I", b"VPK1", 1, item["id"], code[5], len(image), binascii.crc32(image)
        )
        (dest / (name + ".vpk")).write_bytes(header + image)
    if only:
        return
    from package_format import pack

    (out / "catalog.bin").write_bytes(
        pack([(dest / (n + ".vpk")).read_bytes() for n in names])
    )
