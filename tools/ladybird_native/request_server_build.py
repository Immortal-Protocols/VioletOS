"""Build the TLS transport used by the pinned RequestServer."""
import hashlib

from build_ladybird_media import aarch64_cc
from build_runtime import ndk_bin, run


def build_curl(work, native, sysroot, upstream, gcc):
    source = native / 'curl-8.23.0'
    patch = upstream / 'Meta/CMake/vcpkg/overlay-ports/curl/wake-all-pending-transfers.patch'
    stamp = source / '.ladybird-patch'
    if not stamp.is_file():
        run('patch', '-s', '-p1', '-i', patch, cwd=source)
        stamp.write_text(hashlib.sha256(patch.read_bytes()).hexdigest())
    out = work / 'curl'
    tools = ndk_bin()
    lib = sysroot / 'usr/lib'
    headers = sysroot / 'usr/include'
    run('cmake', '-S', source, '-B', out, '-GNinja', '-DCMAKE_BUILD_TYPE=Release',
        '-DCMAKE_SYSTEM_NAME=Linux', '-DCMAKE_SYSTEM_PROCESSOR=aarch64',
        f'-DCMAKE_C_COMPILER={aarch64_cc()}', f'-DCMAKE_AR={tools / "llvm-ar"}',
        f'-DCMAKE_RANLIB={tools / "llvm-ranlib"}',
        f'-DCMAKE_C_FLAGS=-I{headers} -I{native}/ngtcp2-1.26.0/crypto/includes -ffunction-sections -fdata-sections',
        f'-DCMAKE_EXE_LINKER_FLAGS={gcc}', '-DCMAKE_DISABLE_FIND_PACKAGE_PkgConfig=ON',
        '-DBUILD_SHARED_LIBS=OFF', '-DBUILD_CURL_EXE=OFF', '-DBUILD_TESTING=OFF',
        '-DBUILD_EXAMPLES=OFF', '-DBUILD_LIBCURL_DOCS=OFF', '-DBUILD_MISC_DOCS=OFF',
        '-DCURL_USE_OPENSSL=ON', '-DCURL_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt',
        '-DCURL_CA_PATH=none', f'-DOPENSSL_INCLUDE_DIR={headers}',
        f'-DOPENSSL_SSL_LIBRARY={lib}/libssl.a', f'-DOPENSSL_CRYPTO_LIBRARY={lib}/libcrypto.a',
        '-DCURL_ZLIB=ON', f'-DZLIB_INCLUDE_DIR={headers}', f'-DZLIB_LIBRARY={lib}/libz.a',
        '-DCURL_BROTLI=OFF', '-DCURL_ZSTD=OFF', '-DUSE_NGHTTP2=ON', '-DUSE_NGTCP2=ON',
        f'-DNGHTTP2_INCLUDE_DIR={headers}', f'-DNGHTTP2_LIBRARY={lib}/libnghttp2.a',
        f'-DNGHTTP3_INCLUDE_DIR={headers}', f'-DNGHTTP3_LIBRARY={lib}/libnghttp3.a',
        f'-DNGTCP2_INCLUDE_DIR={headers}', f'-DNGTCP2_LIBRARY={lib}/libngtcp2.a',
        f'-DNGTCP2_CRYPTO_OSSL_LIBRARY={work}/libngtcp2_crypto_ossl.a',
        '-DCURL_DISABLE_LDAP=ON', '-DCURL_DISABLE_LDAPS=ON', '-DCURL_USE_LIBPSL=OFF',
        '-DCURL_USE_LIBSSH2=OFF', '-DCURL_USE_LIBSSH=OFF', '-DUSE_LIBIDN2=OFF',
        '-DCURL_USE_GSSAPI=OFF', '-DCMAKE_DISABLE_FIND_PACKAGE_Perl=ON', '-DUSE_HTTPSRR=OFF', '-DUSE_ECH=OFF')
    run('cmake', '--build', out, '--target', 'libcurl_static', '--parallel', '8')
    return out / 'lib/libcurl.a'


def build_avif(work, native, sysroot):
    source = native / 'libavif-1.4.2'
    out = work / 'libavif'
    tools = ndk_bin()
    run('cmake', '-S', source, '-B', out, '-GNinja', '-DCMAKE_BUILD_TYPE=Release',
        '-DCMAKE_SYSTEM_NAME=Linux', '-DCMAKE_SYSTEM_PROCESSOR=aarch64',
        f'-DCMAKE_C_COMPILER={aarch64_cc()}', f'-DCMAKE_AR={tools / "llvm-ar"}',
        f'-DCMAKE_RANLIB={tools / "llvm-ranlib"}', f'-DCMAKE_PREFIX_PATH={sysroot}/usr',
        f'-DCMAKE_C_FLAGS=-I{sysroot}/usr/include -ffunction-sections -fdata-sections',
        '-DBUILD_SHARED_LIBS=OFF', '-DAVIF_CODEC_AOM=SYSTEM', '-DAVIF_CODEC_AOM_ENCODE=OFF',
        '-DAVIF_CODEC_AOM_DECODE=ON', '-DAVIF_LIBYUV=OFF', '-DAVIF_BUILD_APPS=OFF',
        '-DAVIF_BUILD_TESTS=OFF', '-DAVIF_ENABLE_WERROR=OFF', '-DAVIF_LIBSHARPYUV=OFF')
    run('cmake', '--build', out, '--parallel', '8')
    return out / 'libavif.a'
