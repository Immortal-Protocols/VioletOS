"""The kernel's differences from upstream seL4, checked against delta.json.

VioletOS does not run a verified seL4 configuration (README.md). What it can
do is keep the list of differences exact: delta.json names every upstream file
the build changes, every file it adds, every source in this repository that
enters the kernel (with its SHA-256) and every build option it sets, with its
value. The tree comparison checks content, not only names: a changed file must
be exactly the pinned upstream file with the listed patch or script applied,
and an added file must be exactly its listed source.

    python3 kernel/sel4/delta.py            check the repository against delta.json
    python3 kernel/sel4/delta.py --update   record the current source hashes
    python3 kernel/sel4/delta.py --tree build/sel4/qemu/upstream \
        --archive build/sel4/downloads/seL4-17.0.0.tar.gz
                                            compare a prepared kernel tree with the pinned archive

kernel/sel4/build.py runs the tree comparison on every build, and after the
kernel links it checks that only the listed stubs contain an SMC instruction.
"""
import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
MANIFEST = HERE / "delta.json"


def load(path=MANIFEST):
    return json.loads(Path(path).read_text())


def ignored(relative):
    # Byte-code caches written when the kernel's own Python tools run.
    return "__pycache__" in relative.split("/") or relative.endswith(".pyc")


def archive_hashes(archive):
    """{path: sha256} of the pinned source archive, without its top directory."""
    hashes = {}
    with tarfile.open(archive) as tar:
        for member in tar:
            name = member.name.split("/", 1)[1] if "/" in member.name else ""
            if not name or name == "pax_global_header":
                continue
            if member.isfile():
                hashes[name] = hashlib.sha256(tar.extractfile(member).read()).hexdigest()
            elif member.issym() or member.islnk():
                hashes[name] = "link:" + member.linkname
    return hashes


def tree_hashes(root):
    hashes = {}
    root = Path(root)
    for directory, folders, files in os.walk(root):
        # A link to a directory is an entry of its own (os.walk does not follow it).
        for name in files + [f for f in folders if (Path(directory) / f).is_symlink()]:
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if ignored(relative):
                continue
            if path.is_symlink():
                hashes[relative] = "link:" + os.readlink(path)
            else:
                hashes[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    return hashes


def expected_hashes(manifest, archive, repo=REPO, derived=None):
    """{path: sha256} of what every changed and added file must contain.

    A changed file is the pinned upstream file after the patch or script named
    by its "from" has run on a pristine copy, here, in a scratch directory. An
    added file is its "from" source in this repository, or the upstream file
    named by "copy_of". `derived` gives the hash of a file the build derives
    from something outside the repository (the handset's base device tree,
    decompiled from the factory boot image); without it such a file must be
    its "from".
    """
    repo, expected = Path(repo), {}
    by_origin = {}
    for path, entry in manifest["changed"].items():
        by_origin.setdefault(entry.get("from"), []).append(path)
    wanted = {p for paths in by_origin.values() for p in paths}
    copies = {entry["copy_of"] for entry in manifest["added"].values() if entry.get("copy_of")}
    with tempfile.TemporaryDirectory() as scratch, tarfile.open(archive) as tar:
        scratch = Path(scratch)
        for member in tar:
            name = member.name.split("/", 1)[1] if "/" in member.name else ""
            if member.isfile() and name in wanted | copies:
                data = tar.extractfile(member).read()
                if name in copies:
                    expected["upstream:" + name] = hashlib.sha256(data).hexdigest()
                if name in wanted:
                    (scratch / name).parent.mkdir(parents=True, exist_ok=True)
                    (scratch / name).write_bytes(data)
        for origin, paths in by_origin.items():
            source = repo / origin if origin else None
            if not source or not source.is_file() or any(not (scratch / p).is_file() for p in paths):
                continue  # source_errors and tree_errors report a missing origin or upstream file
            if origin.endswith(".patch"):
                command = ["git", "apply", str(source.resolve())]
            else:
                command = [sys.executable, str(source.resolve()), str(scratch)]
            # GIT_CEILING_DIRECTORIES: the scratch directory is never inside a repository.
            result = subprocess.run(command, cwd=scratch, capture_output=True, text=True,
                                    env={**os.environ, "GIT_CEILING_DIRECTORIES": str(scratch.parent)})
            if result.returncode:
                raise SystemExit(f"{origin} does not apply to the pinned upstream files: {result.stderr.strip()}")
            for path in paths:
                expected[path] = hashlib.sha256((scratch / path).read_bytes()).hexdigest()
    for path, entry in manifest["added"].items():
        if derived and path in derived:
            expected[path] = derived[path]
        elif entry.get("copy_of"):
            expected[path] = expected.get("upstream:" + entry["copy_of"], "missing upstream")
        elif entry.get("from") and (repo / entry["from"]).is_file():
            expected[path] = hashlib.sha256((repo / entry["from"]).read_bytes()).hexdigest()
    return {path: digest for path, digest in expected.items() if not path.startswith("upstream:")}


def tree_errors(manifest, pristine, tree, expected=None):
    """Differences between a prepared kernel tree and upstream that delta.json does not list.

    With `expected` (expected_hashes), the content of each listed file is
    checked too: being listed does not let a file hold anything else.
    """
    changed = {p for p in pristine if p in tree and tree[p] != pristine[p]}
    added = set(tree) - set(pristine)
    removed = {p for p in pristine if p not in tree and not ignored(p)}
    errors = []
    for path in sorted(changed - set(manifest["changed"])):
        errors.append(f"upstream file changed but not listed: {path}")
    for path in sorted(set(manifest["changed"]) - changed):
        errors.append(f"listed as changed but identical to upstream (or missing): {path}")
    for path in sorted(added - set(manifest["added"])):
        errors.append(f"file added to the kernel tree but not listed: {path}")
    for path in sorted(set(manifest["added"]) - added):
        errors.append(f"listed as added but absent (or present upstream): {path}")
    for path in sorted(removed):
        errors.append(f"upstream file removed: {path}")
    if expected is not None:
        for group, present in (("changed", changed), ("added", added)):
            for path, entry in manifest[group].items():
                origin = entry.get("copy_of") or entry.get("from") or "its origin"
                if path not in present:
                    continue  # reported above
                if path not in expected:
                    errors.append(f"listed as {group} but nothing says what it must contain: {path}")
                elif tree[path] != expected[path]:
                    errors.append(f"listed as {group} but its content is not what {origin} produces: {path}")
    return errors


def patch_targets(text):
    return sorted(set(re.findall(r"^\+\+\+ b/(\S+)", text, re.M)))


def build_options(text):
    """Every -D option kernel/sel4/build.py passes to the kernel's CMake, with how it is set.

    {name: ("value", literal)} for "-DName=literal", and {name: ("expression",
    source)} for "-DName=" + expression. A second setting of an option, or one
    written any other way, raises ValueError: it could not be compared.
    """
    tree = ast.parse(text)
    options, computed = {}, set()

    def note(name, how):
        if name in options:
            raise ValueError(f"-D{name} is set twice")
        options[name] = how

    for node in ast.walk(tree):
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add) and isinstance(node.left, ast.Constant) \
                and isinstance(node.left.value, str) and re.fullmatch(r"-D\w+=", node.left.value):
            computed.add(id(node.left))
            note(node.left.value[2:-1], ("expression", ast.unparse(node.right)))
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value.startswith("-D") and id(node) not in computed:
            match = re.fullmatch(r"-D(\w+)=(.+)", node.value)
            if not match:
                raise ValueError(f"a CMake option written in a form the delta check cannot read: {node.value!r}")
            note(match[1], ("value", match[2]))
    return options


def option_errors(manifest, text):
    """Do the options kernel/sel4/build.py sets match delta.json, names and values?"""
    try:
        options = build_options(text)
    except (SyntaxError, ValueError) as exc:
        return [f"kernel/sel4/build.py: {exc}"]
    errors = []
    if sorted(options) != sorted(manifest["options"]):
        errors.append(f"kernel build options {sorted(options)} differ from delta.json's {sorted(manifest['options'])}")
    for name, (kind, actual) in sorted(options.items()):
        entry = manifest["options"].get(name)
        if entry is None:
            continue
        # A constant is compared with "value"; a computed one with "expression"
        # (its source text), and "value" then describes it in words.
        if ("expression" in entry) != (kind == "expression"):
            errors.append(f"kernel build option {name}: build.py sets a {kind} ({actual}), delta.json records the other kind")
        elif entry.get(kind) != actual:
            errors.append(f"kernel build option {name} is {actual} in build.py, {entry.get(kind)} in delta.json")
    return errors


def source_errors(manifest, repo=REPO):
    """Does the repository still match what delta.json records?"""
    errors = []
    repo = Path(repo)
    for path, digest in manifest["sources"].items():
        file = repo / path
        if not file.is_file():
            errors.append(f"kernel source missing: {path}")
        elif hashlib.sha256(file.read_bytes()).hexdigest() != digest:
            errors.append(f"kernel source changed since delta.json was reviewed: {path}")
    listed = set(manifest["sources"])
    for group in ("changed", "added"):
        for path, entry in manifest[group].items():
            origin = entry.get("from")
            if origin and origin not in listed:
                errors.append(f"{path}: its origin {origin} is not under sources")
            if not entry.get("why"):
                errors.append(f"{path}: no reason given")
    board = repo / "platform/sel4/board"
    if board.is_dir():
        present = {p.relative_to(repo).as_posix() for p in board.rglob("*") if p.is_file() and not ignored(p.as_posix())}
        outside = set(manifest.get("not_in_kernel", {}))
        for path in sorted(present - listed - outside):
            errors.append(f"board file is neither a listed kernel source nor listed under not_in_kernel: {path}")
    for patch in [p for p in manifest["sources"] if p.endswith(".patch")]:
        if (repo / patch).is_file():
            patched = {p for p, e in manifest["changed"].items() if e.get("from") == patch}
            actual = set(patch_targets((repo / patch).read_text()))
            if patched != actual:
                errors.append(f"{patch} touches {sorted(actual)}, delta.json lists {sorted(patched)}")
    loader = repo / "platform/sel4/loader/build.py"
    for path, entry in manifest["added"].items():
        pin = entry.get("handset", {}).get("factory_boot_sha256")
        if pin and loader.is_file() and pin not in loader.read_text():
            errors.append(f"{path}: platform/sel4/loader/build.py does not pin the factory boot image delta.json names")
    script = repo / "kernel/sel4/build.py"
    if script.is_file():
        text = script.read_text()
        errors += option_errors(manifest, text)
        upstream = manifest["upstream"]
        if f'"{upstream["name"]}"' not in text or upstream["sha256"] not in text:
            errors.append("kernel/sel4/build.py does not pin the upstream archive delta.json names")
    return errors


def smc_sites(disassembly):
    """Functions of a disassembled kernel that contain an SMC instruction."""
    sites, symbol = set(), None
    for line in disassembly.splitlines():
        label = re.match(r"^[0-9a-f]+ <([^>]+)>:", line)
        if label:
            symbol = label[1]
        elif re.search(r"\tsmc\b", line):
            sites.add(symbol)
    return sites


def smc_errors(manifest, disassembly):
    allowed = set(manifest["smc"]["functions"])
    return [f"SMC instruction outside the listed stubs: in {name}" for name in sorted(map(str, smc_sites(disassembly) - allowed))]


def handset_base(factory_boot_sha256, device_tree):
    """The `derived` entry for a handset build's base device tree.

    The tree is decompiled from the factory boot image, which is not in this
    repository, so its content is pinned through that image's SHA-256.
    """
    manifest = load()
    for path, entry in manifest["added"].items():
        pin = entry.get("handset", {}).get("factory_boot_sha256")
        if pin:
            if factory_boot_sha256 != pin:
                sys.exit(f"Kernel delta check failed: the factory boot image is not the one kernel/sel4/delta.json pins for {path}")
            return {path: hashlib.sha256(device_tree).hexdigest()}
    sys.exit("Kernel delta check failed: kernel/sel4/delta.json pins no factory boot image")


def enforce_tree(tree, archive, derived=None):
    """Stop the build when the prepared kernel tree has an unlisted difference,
    or a listed file whose content is not what its origin produces."""
    manifest = load()
    archive = Path(archive)
    if not archive.is_file():
        sys.exit(f"{archive} is missing, so the kernel tree cannot be compared with upstream: "
                 f"delete {tree} and build again")
    if hashlib.sha256(archive.read_bytes()).hexdigest() != manifest["upstream"]["sha256"]:
        sys.exit(f"{archive} is not the upstream archive kernel/sel4/delta.json pins")
    errors = source_errors(manifest) + tree_errors(manifest, archive_hashes(archive), tree_hashes(tree),
                                                   expected_hashes(manifest, archive, derived=derived))
    if any("kernel tree" in e or "upstream file" in e or "its content" in e for e in errors):
        errors.append(f"(a stale tree? delete {tree} and build again)")
    if errors:
        sys.exit("Kernel delta check failed (kernel/sel4/delta.json):\n  " + "\n  ".join(errors))


def enforce_smc(disassembly):
    errors = smc_errors(load(), disassembly)
    if errors:
        sys.exit("Kernel delta check failed (kernel/sel4/delta.json):\n  " + "\n  ".join(errors))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--update", action="store_true", help="record the current SHA-256 of every listed source")
    parser.add_argument("--tree", type=Path)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--handset", action="store_true",
                        help="the tree was prepared for the handset: its base device tree comes from the factory image and is not compared")
    args = parser.parse_args()
    manifest = load()
    if args.update:
        for path in manifest["sources"]:
            manifest["sources"][path] = hashlib.sha256((REPO / path).read_bytes()).hexdigest()
        MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    errors = source_errors(manifest)
    if args.tree:
        if not args.archive:
            parser.error("--tree needs --archive")
        tree = tree_hashes(args.tree)
        derived = {p: tree.get(p) for p, e in manifest["added"].items() if e.get("handset")} if args.handset else None
        errors += tree_errors(manifest, archive_hashes(args.archive), tree, expected_hashes(manifest, args.archive, derived=derived))
    if errors:
        sys.exit("\n".join(errors))
    print(f"{len(manifest['changed'])} upstream files changed, {len(manifest['added'])} added, "
          f"{len(manifest['sources'])} sources and {len(manifest['options'])} build options match delta.json.")


if __name__ == "__main__":
    main()
