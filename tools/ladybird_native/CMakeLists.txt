cmake_minimum_required(VERSION 3.25)
project(VioletLibJS LANGUAGES C CXX)

# The regular upstream project configures graphics/media even for the js target.
# Reuse its source lists for the native libraries needed by the standalone VM.
set(upstream "${SOURCES_DIR}/ladybird")
set(generated "${CMAKE_CURRENT_BINARY_DIR}/include")
foreach(lib IN ITEMS AK LibCore LibGC LibJS LibRegex LibTextCodec LibThreading LibUnicode)
    string(REGEX REPLACE "^Lib" "" macro_name "${lib}")
    string(TOUPPER "${macro_name}" macro_name)
    file(MAKE_DIRECTORY "${generated}/${lib}")
    file(WRITE "${generated}/${lib}/Export.h" "#pragma once\n#define ${macro_name}_API\n#define ${macro_name}_NO_EXPORT\n")
endforeach()
configure_file("${upstream}/AK/Debug.h.in" "${generated}/AK/Debug.h" @ONLY)
configure_file("${upstream}/AK/Backtrace.h.in" "${generated}/AK/Backtrace.h" @ONLY)

foreach(dir IN ITEMS AK Libraries/LibGC Libraries/LibUnicode Libraries/LibThreading)
    file(READ "${upstream}/${dir}/CMakeLists.txt" manifest)
    string(REGEX MATCH "set\\(SOURCES[ \r\n]+([^)]*)\\)" match "${manifest}")
    separate_arguments(files NATIVE_COMMAND "${CMAKE_MATCH_1}")
    list(TRANSFORM files PREPEND "${upstream}/${dir}/")
    list(APPEND sources ${files})
endforeach()
foreach(base IN ITEMS ConditionVariablePOSIX LexicalPath MutexPOSIX RWLockPOSIX)
    list(APPEND sources "${upstream}/AK/${base}.cpp")
endforeach()
foreach(base IN ITEMS AnonymousBuffer ElapsedTimer EventLoop EventLoopImplementation
        EventLoopImplementationUnix EventReceiver File StatsLog System ThreadEventQueue Timer)
    list(APPEND sources "${upstream}/Libraries/LibCore/${base}.cpp")
endforeach()

add_library(violet-js-support STATIC ${sources}
    "${SOURCES_DIR}/mimalloc/src/static.c"
    "${SOURCES_DIR}/simdutf/src/simdutf.cpp"
    "${SOURCES_DIR}/zmij/zmij.cc")
target_compile_features(violet-js-support PRIVATE cxx_std_23)
target_compile_options(violet-js-support PRIVATE -ffunction-sections -fdata-sections
    "$<$<COMPILE_LANGUAGE:CXX>:-fno-exceptions>" "$<$<COMPILE_LANGUAGE:CXX>:-nostdinc++>")
target_compile_definitions(violet-js-support PRIVATE _GNU_SOURCE MI_STATIC_LIB LIBGC_STATIC_DEFINE)
target_include_directories(violet-js-support SYSTEM PRIVATE
    "$<$<COMPILE_LANGUAGE:CXX>:${CMAKE_SYSROOT}/usr/include/c++/15.2.0>"
    "$<$<COMPILE_LANGUAGE:CXX>:${CMAKE_SYSROOT}/usr/include/c++/15.2.0/aarch64-alpine-linux-musl>")
target_include_directories(violet-js-support PRIVATE
    "${upstream}" "${upstream}/Libraries" "${generated}" "${generated}/LibGC"
    "${SOURCES_DIR}/fast_float/include" "${SOURCES_DIR}/llvm-project"
    "${SOURCES_DIR}/mimalloc/include" "${SOURCES_DIR}/mimalloc/src"
    "${SOURCES_DIR}/simdutf/include" "${SOURCES_DIR}/simdutf/src"
    "${SOURCES_DIR}/zmij")
