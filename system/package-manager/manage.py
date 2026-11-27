"""Stage signed component packages. QEMU activates the catalog on its next boot."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
from contextlib import contextmanager

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "image/packages"))
from package_format import floors, inspect, pack, unpack, version
from sign import load_keys

COMPONENTS = json.loads((ROOT / "image/catalog/components.json").read_text())
NAMES = {c["name"]: c for c in COMPONENTS}


@contextmanager
def locked(directory):
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / "lock").open("a+") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        yield


def atomic(path, data):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as f:
        try:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
            os.replace(f.name, path)
            directory_fd = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        finally:
            Path(f.name).unlink(missing_ok=True)


def read_state(directory):
    path = directory / "active.json"
    return (
        json.loads(path.read_text())
        if path.exists()
        else {"current": None, "previous": None}
    )


def catalog_path(directory, digest):
    if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
        raise ValueError("invalid catalog digest")
    return directory / (digest + ".bin")


def launch_catalogs(directory):
    state = read_state(directory)
    return tuple(
        catalog_path(directory, v) if v else None
        for v in (state["current"], state["previous"])
    )


def acknowledge_rollback(directory, launched, source):
    # Compare-and-swap: a late VM shutdown must not undo a newer staged update.
    with locked(directory):
        state = read_state(directory)
        if state["current"] != launched:
            return
        current = state["previous"] if source == 2 else None
        atomic(
            directory / "active.json",
            json.dumps({"current": current, "previous": None}).encode(),
        )


def stage(directory, factory, action, argument):
    with locked(directory):
        state = read_state(directory)
        current, _ = launch_catalogs(directory)
        if action == "rollback":
            if current is None:
                raise ValueError("already running factory catalog")
            state = {"current": state["previous"], "previous": None}
        else:
            installed = (current or factory).read_bytes()
            packages = unpack(installed)
            shipped = unpack(factory.read_bytes()) if current else dict(packages)
            # The highest version seen of each component, kept in the catalog
            # itself so that removing a component does not forget it.
            kept = floors(installed)
            if action == "install":
                data = Path(argument).read_bytes()
                # The same checks the supervisor makes at boot: a signature by a
                # key the image pins, and no version older than the one installed
                # or the one the image shipped with.
                pinned = factory.parent / "package-keys.json"
                if not pinned.is_file():
                    raise ValueError(f"cannot verify the package: {pinned} is missing (build the image first)")
                target, keys = load_keys(pinned)
                identity = inspect(data, keys, target)
                if identity == 21:
                    raise ValueError("USB Diagnostics requires the Moto target")
                for catalog in (packages, shipped):
                    if identity in catalog and version(data) < version(catalog[identity]):
                        raise ValueError("package is older than the installed version")
                if version(data) < kept.get(identity, 0):
                    raise ValueError("package is older than the installed version (a version that was removed)")
                packages[identity] = data
            else:
                item = NAMES[argument]
                if item["required"]:
                    raise ValueError(
                        "boot-critical component requires a compatible replacement"
                    )
                if item["id"] not in packages:
                    raise ValueError("component is already absent")
                kept[item["id"]] = max(kept.get(item["id"], 0), version(packages[item["id"]]))
                del packages[item["id"]]
            for identity, blob in packages.items():
                kept[identity] = max(kept.get(identity, 0), version(blob))
            above = {i: v for i, v in kept.items() if i not in shipped or v > version(shipped[i])}
            data = pack(packages.values(), above)
            digest = hashlib.sha256(data).hexdigest()
            if digest == state["current"]:
                return
            atomic(catalog_path(directory, digest), data)
            state = {"current": digest, "previous": state["current"]}
        atomic(directory / "active.json", json.dumps(state).encode())


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--state-dir", type=Path, default=ROOT / "build/sel4/qemu/installed")
    p.add_argument("--factory", type=Path, default=ROOT / "build/sel4/qemu/catalog.bin")
    sub = p.add_subparsers(dest="action", required=True)
    sub.add_parser("install").add_argument("package", type=Path)
    sub.add_parser("uninstall").add_argument("component", choices=NAMES)
    sub.add_parser("rollback")
    sub.add_parser("list")
    args = p.parse_args()
    if args.action == "list":
        current, _ = launch_catalogs(args.state_dir)
        blobs = unpack((current or args.factory).read_bytes())
        for component in COMPONENTS:
            if component["id"] in blobs:
                print(
                    component["name"],
                    hashlib.sha256(blobs[component["id"]]).hexdigest()[:12],
                )
    else:
        stage(
            args.state_dir,
            args.factory,
            args.action,
            getattr(args, "package", getattr(args, "component", None)),
        )
        print("Catalog staged for next QEMU boot.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as exc:
        sys.exit(str(exc))
