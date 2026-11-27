"""Embed an explicitly selected directory as immutable guest files."""


def pack(root, out, program_name):
    from pathlib import PurePosixPath
    entries = []
    directories = {str(path) for path in PurePosixPath(program_name).parents}
    directories.update(("/dev", "/dev/pts", "/dev/violet", "/proc", "/proc/self", "/proc/self/task",
                        "/proc/self/fd", "/proc/sys", "/proc/sys/vm", "/tmp"))
    with (out / "files.bin").open("wb") as data:
        if root:
            root = root.resolve(strict=True)
            for item in sorted(root.rglob("*")):
                resolved = item.resolve(strict=True)
                if not resolved.is_relative_to(root):
                    raise ValueError(f"bundle link leaves the input directory: {item}")
                if resolved.is_dir():
                    directories.add("/" + str(item.relative_to(root)))
                    continue
                if not resolved.is_file():
                    continue
                name = "/" + str(item.relative_to(root))
                if name == program_name:
                    continue  # the primary ELF already has its own symbols
                blob = resolved.read_bytes()
                data.write(bytes(-data.tell() % 4096))
                start = data.tell()
                data.write(blob)
                entries.append((name, start, len(blob)))
    asm = ['.balign 8', '.global fs_begin, fs_end', 'fs_begin:',
           '.quad program_path, guest_start, guest_end - guest_start']
    for i, (_, start, size) in enumerate(entries):
        asm.append(f'.quad path_{i}, bundle_data+{start}, {size}')
    asm.extend(['fs_end:', 'program_path: .byte ' + ','.join(str(x) for x in program_name.encode() + b"\0")])
    for i, (name, _, _) in enumerate(entries):
        asm.append(f'path_{i}: .byte ' + ','.join(str(x) for x in name.encode() + b"\0"))
    directories = sorted(directories)
    asm.extend(['.balign 8', '.global fs_dirs, fs_dirs_end', 'fs_dirs:'])
    for i in range(len(directories)):
        asm.append(f".quad dir_{i}, 0, 0")
    asm.append("fs_dirs_end:")
    for i, path in enumerate(directories):
        asm.append(f"dir_{i}: .byte " + ",".join(str(x) for x in path.encode() + b"\0"))
    asm.extend(['.balign 4096', '.global bundle_data, bundle_end', 'bundle_data: .incbin "files.bin"', 'bundle_end:'])
    return "\n".join(asm) + "\n"
