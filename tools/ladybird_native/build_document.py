"""Cross-build the upstream LibWeb document helper and a guest overlay for AArch64 VioletOS.

The resulting usr/libexec/ladybird-dom reads HTML from a file or stdin, executes
inline scripts in the page realm, and prints innerText or the serialized DOM.
The host requires the dependencies of build_runtime.py, Python 3.12+, and make.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tarfile

from build_runtime import HERE, TARGET, build as build_runtime, download, ndk_bin, run
from build_ladybird_media import build as build_ffmpeg
from document_codegen import copy_rust_headers, generate
from request_server_build import build_avif, build_curl

MODULES = """LibAudio LibCompositing LibCompress LibCore LibCrypto LibDatabase LibFileSystem LibGC LibGfx
LibHTTP LibIPC LibImageDecoderClient LibJS LibMain LibMedia LibMediaClient LibMediaCodecs LibPlatformFeatures
LibRegex LibRequests LibSandbox LibSyntax LibTLS LibTextCodec LibThreading LibURL LibWeb LibWebCommon LibWebRTCClient LibXML""".split()
LIBRARIES = """harfbuzz freetype png16 jpeg webp psl crypto ssl sqlite3 glib-2.0 gobject-2.0 pcre2-8
ffi zstd z expat bz2 idn2 unistring brotlidec brotlicommon graphite2 woff2""".split()


def prepare_dependencies(work):
    manifest = HERE / 'document-sources.json'
    entries = json.loads(manifest.read_text())
    cache = work / 'downloads'
    cache.mkdir(parents=True, exist_ok=True)
    with ThreadPoolExecutor(max_workers=6) as executor:
        archives = list(executor.map(lambda e: download(e, cache), entries))
    root = work / 'sysroot'
    sources = work / 'dependencies'
    fonts = work / 'root/usr/share/fonts'
    fonts.mkdir(parents=True, exist_ok=True)
    root.mkdir(exist_ok=True)
    stamp = root / '.prepared'
    digest = hashlib.sha256(manifest.read_bytes()).hexdigest()
    if stamp.is_file() and stamp.read_text() == digest:
        return root, sources, cache / 'transport_security_state_static.json'
    for entry_info, item in zip(entries, archives):
        if item.suffix == '.apk':
            with tarfile.open(item) as archive:
                for entry in archive:
                    if not entry.isfile():
                        continue
                    if entry.name == 'etc/ssl/certs/ca-certificates.crt':
                        dest = work / 'root' / entry.name
                    elif entry.name.endswith(('-Regular.ttf', '-Bold.ttf')):
                        dest = fonts / Path(entry.name).name
                    elif entry.name.startswith(('usr/include/', 'usr/lib/')) and '.so' not in entry.name:
                        dest = root / entry.name
                    else:
                        continue
                    dest.parent.mkdir(exist_ok=True, parents=True)
                    data = archive.extractfile(entry).read()
                    if not dest.is_file() or dest.read_bytes() != data:
                        dest.write_bytes(data)
        elif item.name == 'wuffs-v0.3.c':
            dest = root / 'usr/include/wuffs' / item.name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(item, dest)
        elif item.suffix == '.tgz' or item.name.endswith('.tar.xz'):
            dest = sources / item.name.removesuffix('.tgz').removesuffix('.tar.xz')
            dest.mkdir(parents=True, exist_ok=True)
            source_stamp = dest / '.source-sha256'
            if not source_stamp.exists() or source_stamp.read_text() != entry_info['sha256']:
                run('tar', '-xf', item, '-C', dest, '--strip-components', '1')
                (dest / '.ladybird-patch').unlink(missing_ok=True)
                source_stamp.write_text(entry_info['sha256'])
    stamp.write_text(digest)
    return root, sources, cache / 'transport_security_state_static.json'


def compile_sources(work, upstream, support, deps, native, ffmpeg, rustlib, rust_sysroot):
    generated = work / 'include'
    sysroot = support / 'sources/sysroot'
    cxx_headers = next((sysroot / 'usr/include/c++').iterdir())
    tools = ndk_bin()
    cflags = [f'--target={TARGET}', f'--sysroot={sysroot}', '-O1', '-ffunction-sections', '-fdata-sections']
    cxxflags = [*cflags, '-std=c++23', '-fno-exceptions', '-nostdinc++', '-isystem', cxx_headers,
                '-isystem', cxx_headers / 'aarch64-alpine-linux-musl', '-D_GNU_SOURCE',
                '-Wno-unqualified-std-cast-call', '-Wno-user-defined-literals', '-Wno-invalid-offsetof' ,'-Wno-unknown-warning-option']
    includes = [upstream, upstream / 'Libraries', upstream / 'Services', generated, support / 'cpp/include', support / 'cpp/include/LibGC',
                support / 'sources/mimalloc/include', support / 'sources/simdutf/include', generated / 'LibGC',
                deps / 'usr/include', deps / 'usr/include/libpng16', deps / 'usr/include/freetype2', ffmpeg / 'include']
    def quoted(args):
        return shlex.join(map(str, args)).replace('$', '$$')
    def path(value):
        return str(value).replace('$', '$$').replace(' ', '$ ').replace(':', '$:')
    out = work / 'objects'
    out.mkdir(exist_ok=True)
    lines = ['rule compile', '  command = $compiler $flags -MD -MF $out.d -c $in -o $out',
             '  description = CXX $in', '  deps = gcc', '  depfile = $out.d', 'rule archive',
             f'  command = cmake -E rm -f $out && {quoted([tools / "llvm-ar"])} rcs $out @$out.rsp',
             '  description = AR $out', '  rspfile = $out.rsp', '  rspfile_content = $in']
    def compile_one(source, compiler, flags):
        name = hashlib.sha256(str(source).encode()).hexdigest()[:16]
        obj = out / f'{source.stem}-{name}.o'
        lines.extend([f'build {path(obj)}: compile {path(source)}', f'  compiler = {quoted([compiler])}',
                      f'  flags = {quoted(flags)}'])
        return obj
    c = os.environ.get('CC', 'clang')
    cxx = os.environ.get('CXX', 'clang++')
    def archive(target, objects):
        lines.append(f'build {path(target)}: archive ' + ' '.join(path(obj) for obj in objects))
        return target
    libraries = {}
    common_c = ['-I' + str(native / 'brotli/c/include')]
    for lib, directory in [('brotlicommon', 'common'), ('brotlidec', 'dec')]:
        objects = [compile_one(f, c, cflags + common_c) for f in sorted((native / 'brotli/c' / directory).glob('*.c'))]
        libraries[lib] = archive(work / f'lib{lib}.a', objects)
    # Alpine supplies ngtcp2's QUIC transport. Build its corresponding OpenSSL
    # crypto adapter for the same TLS implementation as RequestServer.
    quic = native / 'ngtcp2-1.26.0'
    flags = cflags + ['-DHAVE_ARPA_INET_H', '-DHAVE_NETINET_IN_H', '-DHAVE_SYS_SOCKET_H',
                      '-DHAVE_NETDB_H', '-DHAVE_ENDIAN_H', '-DNGTCP2_STATICLIB', '-DBUILDING_NGTCP2',
                      *('-I' + str(p) for p in [deps / 'usr/include', quic / 'lib', quic / 'crypto', quic / 'crypto/includes'])]
    libraries['ngtcp2_crypto_ossl'] = archive(work / 'libngtcp2_crypto_ossl.a',
        [compile_one(quic / 'crypto' / name, c, flags) for name in ['ossl/ossl.c', 'shared.c']])
    psl = native / 'psl'
    flags = cflags + ['-I' + str(psl / 'include'), '-I' + str(psl / 'src'), '-DENABLE_BUILTIN', '-DWITH_LIBICU',
                      '-DPACKAGE_VERSION="0.23.3"']
    libraries['psl'] = archive(work / 'libpsl.a', [compile_one(f, c, flags) for f in sorted((psl / 'src').glob('*.c'))])
    woff = native / 'woff2-1.0.2'
    flags = cxxflags + ['-include', 'stdint.h', '-I' + str(woff / 'include'), *common_c]
    libraries['woff2'] = archive(work / 'libwoff2.a', [compile_one(woff / 'src' / (n + '.cc'), cxx, flags)
        for n in ['woff2_dec', 'woff2_out', 'variable_length', 'table_tags', 'woff2_common']])
    objects = []
    extra = {
        'LibCore': '''GeolocationProviderUnimplemented AnonymousBuffer EventLoopImplementationUnix FilePosix
            LocalServer MappedFile Process Socket System TCPServer UDPServer FileWatcherInotify Platform/MemoryPressureLinux
            Platform/ProcessStatisticsLinux TimeZoneWatcherUnix'''.split(),
        'LibIPC': ['Attachment', 'TransportSocket'],
        'LibSandbox': ['FileBroker', 'GpuDriverPaths', 'Seccomp'],
        'LibGfx': ['Font/SystemFontMatcherNone'],
        'LibMediaCodecs': ['FFmpeg/SystemFFmpeg'],
        'LibMedia': ['FFmpeg/' + name for name in ['FFmpegAudioConverter', 'FFmpegAudioDecoder', 'FFmpegAudioEncoder',
                                                   'FFmpegChannelLayout', 'SystemFFmpegDecoders']],
    }
    all_cpp = set()
    for module in MODULES:
        folder = upstream / 'Libraries' / module
        manifest = (folder / 'CMakeLists.txt').read_text()
        block = re.search(r'^set\(SOURCES[^\S\n]*\n(.*?)^\)', manifest, re.M | re.S)
        sources = [folder / name for name in re.findall(r'^\s*([\w/]+\.cpp)\s*$', block[1], re.M)]
        sources.extend(folder / (name + '.cpp') for name in extra.get(module, []))
        all_cpp.update(sources)
    view = upstream / 'Libraries/LibWebView'
    all_cpp.update(view / name for name in ['ImageDecoderControlClient.cpp', 'CookieJar.cpp', 'ViewImplementation.cpp', 'WebContentPage.cpp'])
    all_cpp.update(view.glob('Canonical*.cpp'))
    all_cpp.update(generated.rglob('*.cpp'))
    request = upstream / 'Services/RequestServer'
    source_list = re.search(r'^set\(SOURCES\s*\n(.*?)^\)', (request / 'CMakeLists.txt').read_text(), re.M | re.S)
    all_cpp.update(request / name for name in source_list[1].split())
    all_cpp.add(request / 'SandboxLinux.cpp')
    all_cpp.add(upstream / 'Libraries/LibCore/CrashHandlerPOSIX.cpp')
    all_cpp.update((upstream / 'Libraries/LibDNS').glob('*.cpp'))
    all_cpp.update((upstream / 'Libraries/LibWebSocket').rglob('*.cpp'))
    image_service = upstream / 'Services/ImageDecoder'
    all_cpp.update(image_service / name for name in ['ConnectionFromClient.cpp', 'ControlConnection.cpp', 'SandboxLinux.cpp'])
    image_formats = upstream / 'Libraries/LibGfx/ImageFormats'
    decoder_manifest = (upstream / 'Libraries/LibImageDecoders/CMakeLists.txt').read_text()
    all_cpp.update(image_formats / name for name in re.findall(r'\$\{LIBGFX_IMAGE_FORMATS_DIR\}/(\w+\.cpp)', decoder_manifest))
    for source in sorted(all_cpp):
        mod = next((part for part in source.parts if part in MODULES), 'LibCore')
        flags = cxxflags + ['-I' + str(generated / mod)] + ['-I' + str(i) for i in includes]
        if source.is_relative_to(image_formats):
            flags += ['-I' + str(native / 'libavif-1.4.2/include'), '-I' + str(deps / 'usr/include/wuffs')]
        if 'RequestServer' in source.parts:
            flags.append('-I' + str(native / 'curl-8.23.0/include'))
        if mod == 'LibSandbox' and source.name == 'Seccomp.cpp':
            # musl's ancillary-data field uses socklen_t. The one narrowing conversion
            # counts a verified maximum of three fds. The ioctl masks are Linux UAPI.
            flags += ['-include', 'asm/ioctl.h', '-Wno-c++11-narrowing']
        if mod == 'LibURL':
            flags += ['-DENABLE_RUST']
        objects.append(compile_one(source, cxx, flags))
    engine = archive(work / 'libweb.a', objects)
    entry = compile_one(HERE / 'document.cpp', cxx, cxxflags + ['-I' + str(i) for i in includes])
    service_entry = compile_one(request / 'main.cpp', cxx, cxxflags + ['-I' + str(i) for i in includes]
                                + ['-I' + str(native / 'curl-8.23.0/include')])
    image_entry = compile_one(image_service / 'main.cpp', cxx, cxxflags + ['-I' + str(i) for i in includes])
    (work / 'build.ninja').write_text('\n'.join(lines) + '\n')
    run('ninja', '-C', work, '-j', str(min(12, os.cpu_count() or 2)), entry, service_entry, image_entry, engine, *libraries.values())
    crt = rust_sysroot / 'lib/rustlib' / TARGET / 'lib/self-contained'
    lib = sysroot / 'usr/lib'
    libs = [libraries.get(n, deps / 'usr/lib' / ('lib' + n + '.a')) for n in LIBRARIES]
    elf = work / 'root/usr/libexec/ladybird-dom'
    elf.parent.mkdir(parents=True, exist_ok=True)
    rust_libraries = ['web_bindings', 'compositing', 'gc', 'gfx', 'imagedecoders', 'regex', 'textcodec',
                      'unicode', 'url', 'web_content_blocker', 'webcommon', 'xml']
    common = [*(rustlib / ('liblib' + name + '_rust.a') for name in rust_libraries),
        support / 'cpp/libviolet-js-support.a', *libs,
        *sorted((ffmpeg / 'lib').glob('*.a')), *(lib / ('lib' + n + '.a') for n in ['icui18n', 'icuuc', 'icudata', 'stdc++']),
        *lib.glob('gcc/*/*/libgcc.a'), crt / 'libc.a', crt / 'libunwind.a', '--end-group', crt / 'crtend.o', crt / 'crtn.o']
    service = work / 'root/opt/ladybird/libexec/RequestServer'
    service.parent.mkdir(parents=True, exist_ok=True)
    curl = build_curl(work, native, deps, upstream, next(lib.glob('gcc/*/*/libgcc.a')))
    transport = [curl, libraries['ngtcp2_crypto_ossl'],
                 *(deps / 'usr/lib' / ('lib' + n + '.a') for n in ['ngtcp2', 'nghttp3', 'nghttp2'])]
    avif = build_avif(work, native, deps)
    image_libs = [avif, *(deps / 'usr/lib' / ('lib' + n + '.a') for n in
                         ['aom', 'webpdecoder', 'webpdemux', 'webpmux', 'sharpyuv'])]
    for program, main, extra in [(elf, entry, []), (service, service_entry, transport),
                                 (service.with_name('ImageDecoder'), image_entry, image_libs)]:
        run(tools / 'ld.lld', '-static', '-s', '--gc-sections', '--allow-multiple-definition', '-o', program,
            crt / 'crt1.o', crt / 'crti.o', crt / 'crtbegin.o', main, '--start-group', engine, *extra, *common)
    return elf


def build(work, support):
    build_runtime(support)
    deps, native, hsts = prepare_dependencies(work)
    upstream = support / 'sources/ladybird'
    generated = work / 'include'
    generated.mkdir(exist_ok=True)
    generator_stamp = generated / '.generated'
    state = hashlib.sha256((HERE / 'document_codegen.py').read_bytes() + (HERE / 'sources.json').read_bytes() + hsts.read_bytes()).hexdigest()
    if not generator_stamp.exists() or generator_stamp.read_text() != state:
        generate(upstream, generated, hsts)
        generator_stamp.write_text(state)
    rustc = os.environ.get('RUSTC', 'rustc')
    rust_sysroot = Path(subprocess.check_output([rustc, '--print', 'sysroot'], text=True).strip())
    target = support / 'cargo'
    env = dict(os.environ, RUSTC=rustc, CARGO_TARGET_DIR=str(target),
               LIBWEB_RUST_BINDINGS=str(generated / 'LibWeb/Bindings/RustBindings.rs'),
               CC_aarch64_unknown_linux_musl=os.environ.get('CC', 'clang'),
               AR_aarch64_unknown_linux_musl=str(ndk_bin() / 'llvm-ar'),
               CFLAGS_aarch64_unknown_linux_musl=f'--target={TARGET} --sysroot={support / "sources/sysroot"}',
               LADYBIRD_LINKS_AK='1')
    env['DYLD_FALLBACK_LIBRARY_PATH'] = os.pathsep.join(filter(None, [str(rust_sysroot / 'lib'), env.get('DYLD_FALLBACK_LIBRARY_PATH')]))
    crates = '''libweb_bindings_rust libweb_content_blocker_rust libjs_embedding_headers libxml_rust
        libtextcodec_rust libgfx_rust libregex_rust liburl_rust libcompositing_rust libwebcommon_rust libimagedecoders_rust'''.split()
    proc = run(os.environ.get('CARGO', 'cargo'), 'build', '--manifest-path', upstream / 'Cargo.toml',
               '--locked', '--no-default-features', '--release', '--target', TARGET, '--message-format=json-render-diagnostics',
               *(arg for name in crates for arg in ['-p', name]), env=env, text=True, stdout=subprocess.PIPE)
    copy_rust_headers((json.loads(line) for line in proc.stdout.splitlines()), generated, upstream)
    icu = work / 'root/usr/share/icu/78.3'
    icu.mkdir(parents=True, exist_ok=True)
    shutil.copy2(support / 'sources/sysroot/usr/share/icu/78.3/icudt78l.dat', icu)
    elf = compile_sources(work, upstream, support, deps, native, build_ffmpeg('aarch64'), target / TARGET / 'release', rust_sysroot)
    print(f'Built {elf}. Overlay {work / "root"} on the guest filesystem.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    base = HERE.parents[1] / 'build/ladybird-native'
    parser.add_argument('--build-dir', type=Path, default=base / 'document')
    parser.add_argument('--support-dir', type=Path, default=base / 'static')
    args = parser.parse_args()
    build(args.build_dir.resolve(), args.support_dir.resolve())
