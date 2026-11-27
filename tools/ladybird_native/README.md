# Native Ladybird document engine

The AArch64 VioletOS browser can pipe fetched HTML into the upstream LibWeb renderer and read back the DOM after parsing and script execution. LibWeb starts a separate RequestServer inside VioletOS and starts ImageDecoder on demand. The processes use upstream IPC, cookie handling and the Fetch implementation. RequestServer uses libcurl, OpenSSL and the guest CA bundle.

The pinned Ladybird revision uses Rust 1.98.0 with the AArch64-musl target. Install clang, the Android NDK, CMake, Ninja, make, and Python 3.12 or later. The upstream source, native dependencies and fonts are checksum-pinned.

```sh
rustup toolchain install 1.98.0 --target aarch64-unknown-linux-musl
RUSTUP_TOOLCHAIN=1.98.0 python3 tools/ladybird_native/build_document.py
```

Merge the contents of `build/ladybird-native/document/root` into the guest root when packaging the graphical browser. The overlay includes the document process at `/usr/libexec/ladybird-dom`, RequestServer and ImageDecoder under `/opt/ladybird/libexec`, Liberation fonts, ICU data and trusted certificate authorities.

Within the guest, run the following command with the HTML page on standard input. A filename can also be supplied. Omitting `--html` returns the rendered body’s inner text.

```sh
/usr/libexec/ladybird-dom --html --url https://example.org/article -
```

When using this source-built preview, the graphical frontend creates a document process per navigation and imports the returned HTML. If a script or iframe requests a new document, the helper cancels its pending navigation and exits with status 1 before serializing. The graphical frontend can then display the original HTML. Select the full Hub browser to navigate child frames with upstream site isolation. Network fetches run while the document loads. The private cookie jar lives for that document process. Continuous interaction, timers after load, and presenting upstream composited media frames require retaining the page process. RequestServer and ImageDecoder run in separate seL4 address spaces with their upstream Linux seccomp and Landlock policies. The transport supports HTTP/1.1, HTTP/2 and HTTP/3, with TLS verification against the guest CA bundle.

The native regression boots VioletOS, loads HTML, and checks that a separately fetched script sees the document URL and changes the serialized heading, reads the decoded dimensions of a separately fetched PNG, and confirms that cross-site frame navigation exits the preview cleanly. Configure the kernel, Microkit and LLVM paths as described in `targets/qemu/aarch64/README.md`, then run the following command.

```sh
python3 tools/ladybird_native/test_document.py
```
