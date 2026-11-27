"""Run the pinned upstream CMake manifests' source generators without Qt or a host browser."""
from pathlib import Path
import re
import shlex
import shutil
import sys

from build_runtime import run


def generator_calls(manifest, source):
    web = source / 'Libraries/LibWeb'
    text = manifest.read_text().replace('${LIBWEB_INPUT_FOLDER}', str(web)).replace('${LADYBIRD_SOURCE_DIR}', str(source))
    for kind, body in re.findall(r'^\s*(invoke_py_(?:header_|idl_)?generator|embed_as_string)\(\s*(.*?)\s*\)', text, re.M | re.S):
        yield kind, shlex.split(body)


def generate(source, output, hsts):
    generators = source / 'Meta/Generators'
    web = source / 'Libraries/LibWeb'
    for module, manifest in [('LibWeb', source / 'Meta/CMake/libweb_generators.cmake'),
                             ('LibWebCommon', source / 'Libraries/LibWebCommon/CMakeLists.txt')]:
        for kind, args in generator_calls(manifest, source):
            if kind == 'embed_as_string':
                _, resource, name, symbol, _, namespace = args
                dest = output / module / name
                dest.parent.mkdir(parents=True, exist_ok=True)
                run(sys.executable, generators / 'embed_as_string.py', resource, '-o', dest, '-n', symbol, '-s', namespace)
                continue
            header = output / module / args[3]
            header.parent.mkdir(parents=True, exist_ok=True)
            if kind == 'invoke_py_idl_generator':
                command = ['-i', header]
            else:
                command = ['-h', header]
            if kind == 'invoke_py_generator':
                command += ['-c', output / module / args[4]]
            if 'EXTRA_HEADER' in args:
                command += ['-x', output / module / args[args.index('EXTRA_HEADER') + 1]]
            arguments = args[args.index('arguments') + 1:]
            for stop in ['dependencies', 'EXTRA_HEADER']:
                if stop in arguments:
                    arguments = arguments[:arguments.index(stop)]
            run(sys.executable, generators / args[1], *command, *arguments)

    bindings = output / 'LibWeb/Bindings'
    common = output / 'LibWebCommon/Bindings'
    for folder in (bindings, common):
        folder.mkdir(parents=True, exist_ok=True)
    idls = re.findall(r'^libweb_js_bindings\(([^\s)]+)', (web / 'idl_files.cmake').read_text(), re.M)
    inputs = [web / f'{name}.idl' if (web / f'{name}.idl').exists() else output / 'LibWeb' / f'{name}.idl' for name in idls]
    run(sys.executable, generators / 'generate_libweb_bindings.py', '-o', bindings, '--common-output-path', common, *inputs)
    for module in (source / 'Libraries').glob('Lib*'):
        if not module.is_dir():
            continue
        target = output / module.name
        target.mkdir(parents=True, exist_ok=True)
        macro = module.name[3:].upper()
        (target / 'Export.h').write_text(f'#pragma once\n#define {macro}_API\n#define {macro}_NO_EXPORT\n')
    version = (source / 'Libraries/LibCore/BrowserVersion.h.in').read_text()
    for name, value in [('LADYBIRD_BROWSER_VERSION', '2026.47.8'), ('CMAKE_BUILD_TYPE', 'Release'),
                        ('browser_product_name', 'Ladybird'), ('browser_release_stage', ''),
                        ('browser_is_release_build', '0'), ('browser_is_final_release', '0')]:
        version = version.replace('@' + name + '@', value)
    (output / 'LibCore/BrowserVersion.h').write_text(version)
    run(sys.executable, generators / 'generate_hsts_preload_data.py', '-p', hsts,
        '-h', output / 'LibHTTP/HSTSPreloadData.h', '-c', output / 'LibHTTP/HSTSPreloadData.cpp')

    tiff = output / 'LibGfx/ImageFormats'
    tiff.mkdir(parents=True, exist_ok=True)
    run(sys.executable, source / 'Libraries/LibGfx/TIFFGenerator.py', '-o', tiff)

    for cmake in (source / 'Libraries').rglob('CMakeLists.txt'):
        for input_name, out in re.findall(r'^\s*compile_ipc\(\s*([^\s)]+)\s+([^\s)]+)', cmake.read_text(), re.M):
            ipc = (cmake.parent / input_name.replace('${LADYBIRD_SOURCE_DIR}', str(source))).resolve()
            if not ipc.is_file():
                continue
            rel = (cmake.parent / out.replace('${CMAKE_BINARY_DIR}', str(source))).resolve().relative_to(source)
            header = output.joinpath(*rel.parts[1:])
            header.parent.mkdir(parents=True, exist_ok=True)
            run(sys.executable, generators / 'generate_ipc_definitions.py', '--input', ipc, '--output', header,
                '--implementation', header.with_suffix('.cpp'), '--header-name', header.name)


def copy_rust_headers(messages, output, source):
    modules = {p.name.lower() + '_rust': p.name for p in (source / 'Libraries').glob('Lib*')}
    modules.update(libweb_html_tokenizer='LibWeb', libweb_content_blocker_rust='LibWeb', libjs_embedding_headers='')
    for message in messages:
        if message.get('reason') != 'build-script-executed':
            continue
        directory = Path(message['out_dir'])
        crate = directory.parent.name.rsplit('-', 1)[0]
        if crate not in modules or 'aarch64-unknown-linux-musl' not in directory.parts:
            continue
        for path in directory.rglob('*'):
            if path.suffix in {'.h', '.inc'}:
                dest = output / modules[crate] / path.relative_to(directory)
                dest.parent.mkdir(parents=True, exist_ok=True)
                if not dest.is_file() or dest.read_bytes() != path.read_bytes():
                    shutil.copy2(path, dest)
