# Spike 1 · checking a private note page in a seL4 boot image

**Result:** the pre-boot checker accepted the isolated QEMU image and rejected a second image with one extra read mapping. Both boots produced the predicted result. Notes and Network are real, separately scheduled Rust protection domains on seL4, using the Microkit C startup and debug-print runtime. Network is a stand-in for an untrusted service with a packet page; this fixture has no NIC, filesystem, hardware drivers of our own or persistent data.

| Finished image | Before boot | During boot |
| --- | --- | --- |
| Isolated | Network has its own writable packet page and no mapping to the private note frame. | The packet check passed. The seL4 monitor attributed a data fault at 0x40000000 to Network. |
| Deliberate read grant | Checker exited 2 and reported a readable, non-writable Network mapping to the note frame. | The packet check passed. Network read the marker Notes had written. |

Notes runs first at higher priority and stores a synthetic 32-bit value in its private page. Both images use identical input service ELFs. The only system-description change adds a read mapping to Network. Debug names, kernel, SDK loader, initializer and monitor are trusted for this experiment.

## Evidence and limits

The checker reads the raw image intended for QEMU. Using symbols from the pinned SDK, it follows the loader's region table, the initializer's patched program-header pointer and the CapDL segment it references. It validates those bytes with the upstream initializer archive type. The tool-generated JSON and report are provided for human inspection and play no role in the verdict.

Starting at each named thread's VSpace slot, it walks the ARM page tables, identifies the owner's note frame, and checks *all* Network virtual addresses for an alias to that frame. It also follows reachable CNodes and rejects capabilities outside the narrow fixture policy. A malformed or unsupported input exits with an error distinct from the expected policy violation. The harness checks the image hash again before boot.

This is an executable check of an initial object graph plus two runtime observations. The checker, boot loader and initialization code have not been formally verified here. The debug configuration enables printing and debugging facilities. We have no theorem for future states, updates, confidential output, physical frame aliasing on arbitrary systems, timing, DMA or the Moto. The next proof task would be to model permitted transitions and connect the decoded configuration to an applicable kernel theorem.

## Reproduce on an ARM Mac

From the project root, use Python 3.9+, QEMU with qemu-system-aarch64, Rust 1.91.1 with the aarch64-unknown-none target, LLVM nm, and the native Microkit SDK 2.4.0. The SDK and Rust installations for this run live under the ignored build/spike1 directory. Set MICROKIT_SDK to the extracted SDK and LLVM_NM to llvm-nm if it is outside PATH.

```sh
src=spikes/sel4-isolation
out=build/spike1/images
export CARGO_TARGET_DIR="$PWD/build/spike1/checker-target"
sh "$src/build.sh" "$out"
cargo build --locked --manifest-path "$src/checker/Cargo.toml"
python3 "$src/verify.py" "$out" "$CARGO_TARGET_DIR/debug/violet-map-check"
```

The input services have SHA-256 digests 803ff36a88c0635f3d6c61e39f1835f20d13279c50be1cdcc5661ae397f39147 (Notes) and 8d485fdaf81845f502c5a1ed764918ad632b60e5a134ffc98e247af17ff71453 (Network). The tested SDK images have digests 7003f7cc15a8cbe6a758840f1c532104db46bd7d09b5e8121dacf5db5fb12b23 (isolated) and c49e9f03e6fd35448db85a8642c0ecfeab6b6a5d2a755ea5c9b06c387b631e2c (granted). Serial transcripts are saved alongside the images.

## Kernel rebuild with the services pinned

Three timed runs on this Mac used fresh CMake/Ninja output directories, four build jobs, warm sources and installed tools, and disabled compiler caching. Each configured and linked seL4 17.0.0, then packaged an isolated image from the existing service ELFs and SDK boot components. The generated kernel configuration matched the SDK debug configuration exactly. Each resulting image passed the isolated QEMU boot test.

| Run | Configure and compile kernel | Package image | Total |
| --- | ---: | ---: | ---: |
| 1 | 2.151 s | 5.4 ms | 2.157 s |
| 2 | 1.988 s | 5.1 ms | 1.993 s |
| 3 | 2.054 s | 5.2 ms | 2.059 s |

Checkouts, tool downloads, service compilation, checker execution and boot time are outside these timings. The earlier 211.6-second Linux measurement included handset drivers and modules and ran in a Linux container. The two workloads target different boards and provide different functionality.

For the timing, provide an extracted seL4 17.0.0 tree. Put Clang and LLD (tested with Android NDK 28.2.13676358), device-tree compiler, CMake, Ninja, QEMU and the kernel Python tools on PATH. Our Python environment included jinja2, lxml, pyyaml, pyfdt, jsonschema, six, future, ply and setuptools. LLD emitted two section-alignment warnings for the locally built kernel; all three images booted. Further investigation is appropriate before reusing that build on hardware.

```sh
CCACHE_DISABLE=1 CMAKE_POLICY_VERSION_MINIMUM=3.5 python3 spikes/sel4-isolation/bench.py build/spike1/seL4-17.0.0 build/spike1/images
```

[Microkit 2.4.0](https://github.com/seL4/microkit/releases/tag/2.4.0) · [seL4 17.0.0](https://github.com/seL4/seL4/releases/tag/17.0.0) · [Pinned CapDL archive types](https://github.com/seL4/rust-sel4/tree/c819c27b076130cba9821c4555f23773e4b0bffd/crates/sel4-capdl-initializer/types)
