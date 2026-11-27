# Ladybird

On the AArch64 QEMU target, installing a full upstream browser package makes the Home icon launch Ladybird's multiprocess Hub. LibWeb handles scripts, input and page layout, the compositor sends pixels to the VioletOS screen, and RequestServer handles HTTPS inside the guest. Build instructions and the native touch regression are in the [AArch64 guide](../../../targets/qemu/aarch64/README.md).

## Lightweight browser

A web browser for VioletOS, pre-installed as an open app (`/apps/ladybird`, listed on Home as **Ladybird**). Pages are parsed by [Ladybird](https://ladybird.org/)'s own HTML tokenizer and tree builder: the Rust crate in Ladybird's `Libraries/LibWeb/HTML/Parser/Rust`, compiled for the guest and linked into this Rust program. The browser UI, layout and networking are written here in Rust. A small C adapter links FFmpeg for WebM playback.

```
  network (net.rs)                 HTTPS with rustls (violet::http) or HTTP, redirects
        │
  Ladybird's tokenizer + tree builder   (build/ladybird, pinned commit)
        │  ladybird_html_parser_* callbacks
  dom.rs        a Rust DOM that answers them; ak_runtime.rs answers AK's string calls
        │
  style.rs      HTML defaults, <style> and linked sheets, style="" attributes
  layout.rs     blocks, line breaking, lists, tables sized by content
  main.rs       painting, scrolling, links, history, address bar and keyboard
```

## What it does

- Loads `http://` and `https://` pages (certificates verified against the Mozilla roots), follows redirects and fetches up to six linked style sheets.
- Applies CSS with type, class, id and attribute selectors, descendant and child combinators, `@media` width queries for its 360 CSS px viewport, and the common text, color, spacing and `display` properties.
- Lays out headings, paragraphs, lists, block quotes, preformatted text and tables, and draws images as placeholder boxes of their declared size.
- Opens direct `video/webm` responses and shows a **Play video** link for static HTML video sources. The app downloads up to 32 MiB, decodes VP8 or VP9 frames in the guest and sends Opus or Vorbis audio to the VioletOS audio device. Tap the video to pause, resume or replay. Back returns to the page with the video.
- Taps follow links (relative links resolve against the page), dragging scrolls, **‹** goes back, and the address bar opens an on-screen keyboard. Text without a dot or scheme is searched on DuckDuckGo Lite.

The standard ARMv7 image supports static HTML and WebM. The 64-bit QEMU image can run upstream WebContent, LibJS, RequestServer and the software compositor. With a complete matching browser package installed, Home starts the native UI and forwards taps to the live upstream view. With a document-helper package installed, the Rust UI displays the DOM returned by LibWeb after parsing and script execution. A YouTube watch-page playback result requires progressing video frames and audio in the real guest. See the [64-bit guest build and tests](../../../targets/qemu/aarch64/README.md).

## How the Ladybird parser is hosted

In Ladybird, the parser calls into LibWeb's C++ DOM and AK's C++ string runtime. Two files stand in for that C++ here:

- [`dom.rs`](src/dom.rs) defines every `ladybird_html_parser_*` function the tree builder imports and keeps the document in an arena.
- [`ak_runtime.rs`](src/ak_runtime.rs) defines the `ladybird_utf16_*` functions over the same memory layout AK uses (a 24-byte header followed by Latin-1 or UTF-16 storage, with short ASCII strings inline in the word). Fly strings are interned once and never freed, so that the tree builder's identity comparisons hold.

[`build.rs`](build.rs) reads LibWeb's `TagNames.h`, `AttributeNames.h` and the SVG and MathML tag lists, as Ladybird's build does, and defines the `ladybird_*_name_*` statics that the tree builder compares against. They are filled when the browser starts.

## Building

[`tools/fetch_ladybird.py`](../../../tools/fetch_ladybird.py) checks out only the needed paths of one pinned Ladybird commit into `build/ladybird` (about 11 MB). The crate is its own Cargo workspace because Ladybird needs Rust 1.98 (from its `rust-toolchain.toml`) while the other apps use 1.91.1:

```sh
rustup toolchain install 1.98.0 --target armv7-unknown-linux-musleabihf
make qemu-image
```

[`tools/build_initfs.py`](../../../tools/build_initfs.py) fetches Ladybird and builds the browser into `/apps`. The media build cross-compiles FFmpeg 9.0.2 with the Android NDK and the Open Runtime musl sysroot. [Media build](../../../tools/build_ladybird_media.py) Without the toolchain it skips the browser, as it does for other optional toolchains.

On the Mac, `cargo +1.98.0 test --manifest-path apps/open/ladybird/Cargo.toml` runs the unit tests, and a Mac build can render an HTML file the way the phone would: `ladybird --render page.html out.ppm [scroll] [sheet.css…]`.

## Testing

`make test-ladybird` boots QEMU, opens Ladybird from Home and drives it by touch against a local web server standing in for two sites: a link from the start page, a linked style sheet, a relative link, Back, a drag to scroll, and a URL typed on the keyboard, each checked in the Open Runtime's log and the guest's screen pixels. With the Mac online it also loads example.com over HTTPS and reports how a large Wikipedia article loads.

`make test-ladybird-media` opens a static HTML video from Home in a fresh seL4 QEMU guest. The test checks changing VP9 frames, measures an Opus tone in the guest audio ring, then returns to the page and Home. The fixture is served over loopback.
