"""Generate linux.rs: Linux system call numbers for both ABIs, from the kernel's own headers.

    python3 runtime/arch/gen_linux.py            rewrite runtime/arch/linux.rs
    python3 runtime/arch/gen_linux.py --check    fail if linux.rs is not what the headers give

The numbers come from the Linux UAPI headers in the Android NDK the kernel
build already needs (asm/unistd.h for arm and aarch64), read with its clang
preprocessor, so none is typed by hand. What is decided here, by hand, is the
*relation* between the two tables: which AArch32 call the Open Runtime's
dispatcher (runtime/open/syscall.rs, written against AArch32 numbers) can
serve an AArch64 call with, and whether its arguments and structures have the
same layout. Only calls listed below are in the table; an AArch64 call that is
not listed has no entry and a runtime must answer ENOSYS.
"""
import os
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
OUT = HERE / "linux.rs"

# AArch64 call -> the AArch32 call with the same meaning, when the name differs.
# The 64-bit ABI has one variant where AArch32 grew a second, wider one.
RENAMED = {
    "fstat": "fstat64", "newfstatat": "fstatat64", "mmap": "mmap2", "lseek": "_llseek",
    "fcntl": "fcntl64", "statfs": "statfs64", "fstatfs": "fstatfs64", "truncate": "truncate64",
    "ftruncate": "ftruncate64", "sendfile": "sendfile64", "getrlimit": "ugetrlimit",
    "fadvise64": "arm_fadvise64_64", "sync_file_range": "arm_sync_file_range",
    # 64-bit time: AArch64's only variant is AArch32's *_time64 one.
    "clock_gettime": "clock_gettime64", "clock_settime": "clock_settime64",
    "clock_getres": "clock_getres_time64", "clock_nanosleep": "clock_nanosleep_time64",
    "timer_gettime": "timer_gettime64", "timer_settime": "timer_settime64",
    "timerfd_gettime": "timerfd_gettime64", "timerfd_settime": "timerfd_settime64",
    "utimensat": "utimensat_time64", "pselect6": "pselect6_time64", "ppoll": "ppoll_time64",
    "recvmmsg": "recvmmsg_time64", "futex": "futex_time64", "rt_sigtimedwait": "rt_sigtimedwait_time64",
    "sched_rr_get_interval": "sched_rr_get_interval_time64", "io_pgetevents": "io_pgetevents_time64",
    "mq_timedsend": "mq_timedsend_time64", "mq_timedreceive": "mq_timedreceive_time64",
    "semtimedop": "semtimedop_time64",
}

# Calls whose arguments are integers, descriptors, or pointers to bytes or to
# structures made only of fixed-width fields laid out alike on both ABIs. The
# AArch32 handler serves these unchanged once pointers are widened.
SAME = """
read write close exit exit_group getpid gettid getppid getuid geteuid getgid getegid setsid getpgid setpgid
kill tkill tgkill set_tid_address sched_yield brk munmap mprotect madvise mlock munlock msync
dup dup3 pipe2 openat unlinkat mkdirat renameat renameat2 symlinkat linkat readlinkat faccessat faccessat2
fchmod fchmodat fchown fchownat chdir fchdir getcwd umask fsync fdatasync flock sync
getdents64 statx ioctl getrandom eventfd2 epoll_create1 epoll_ctl epoll_pwait
socket bind connect listen accept accept4 getsockname getpeername shutdown setsockopt getsockopt
sendto recvfrom socketpair prctl prlimit64 uname rt_sigprocmask rt_sigpending rt_sigsuspend
memfd_create membarrier sched_getaffinity sched_setaffinity
clock_gettime clock_settime clock_getres clock_nanosleep nanosleep timer_gettime timer_settime
timerfd_create timerfd_gettime timerfd_settime utimensat futex ppoll 
timer_delete timer_getoverrun getpriority setpriority close_range inotify_init1
""".split()

# Calls that exist on both but whose register packing or structure layout
# differs with the word size (iovec, msghdr, stat, sigaction, stack_t, rusage,
# 64-bit offsets, pointer arrays): the handler needs a 64-bit path.
WORDS = """
readv writev preadv pwritev pread64 pwrite64 lseek mmap mremap fstat newfstatat fcntl statfs fstatfs
truncate ftruncate fallocate sendfile sendmsg recvmsg sendmmsg recvmmsg rt_sigaction rt_sigreturn
sigaltstack clone clone3 execve execveat wait4 waitid getrusage getrlimit setrlimit sysinfo
getitimer setitimer gettimeofday settimeofday times fadvise64 sync_file_range rt_sigqueueinfo
rt_tgsigqueueinfo rt_sigtimedwait pselect6 set_robust_list get_robust_list timer_create
""".split()

# nanosleep keeps a 32-bit timespec on AArch32 (there is no nanosleep_time64),
# so it is the one time call whose structure differs.
WORDS.append("nanosleep")
SAME.remove("nanosleep")


def defines(triple, ndk=None):
    ndk = Path(ndk or os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    clangs = list(ndk.glob("toolchains/llvm/prebuilt/*/bin/clang"))
    if not clangs:
        return None
    text = subprocess.check_output([clangs[0], f"--target={triple}", "-E", "-dM", "-x", "c", "-include", "asm/unistd.h", "/dev/null"], text=True)
    raw = dict(re.findall(r"^#define (__NR\w*) (.+)$", text, re.M))

    def value(expression, depth=0):
        if depth > 8:
            raise ValueError(expression)
        expression = re.sub(r"__NR\w*", lambda m: str(value(raw[m[0]], depth + 1)), expression)
        if not re.fullmatch(r"[\d\s()+x a-fA-F]*", expression):
            raise ValueError(expression)
        return eval(expression)  # digits, parentheses and + only (checked above)

    return {name[5:]: value(text) for name, text in raw.items()
            if name.startswith("__NR_") and name not in ("__NR_SYSCALL_BASE", "__NR_OABI_SYSCALL_BASE", "__NR_syscalls", "__NR_arch_specific_syscall")}


def table(ndk=None):
    arm, arm64 = defines("arm-linux-androideabi", ndk), defines("aarch64-linux-android", ndk)
    if arm is None or arm64 is None:
        return None
    rows = []
    for name in sorted(set(SAME) | set(WORDS), key=lambda n: arm64[n]):
        if name in SAME and name in WORDS:
            raise SystemExit(f"{name} is listed as both SAME and WORDS")
        if name not in arm64:
            raise SystemExit(f"{name} is not an AArch64 system call")
        other = RENAMED.get(name, name)
        if other not in arm:
            raise SystemExit(f"{name}: AArch32 has no {other}")
        rows.append((name, arm64[name], other, arm[other], "Same" if name in SAME else "Words"))
    return rows


def render(rows):
    lines = [
        "//! Linux system call numbers of the two ABIs, and how they relate.",
        "//! Generated by gen_linux.py from the Linux UAPI headers: do not edit.",
        "",
        "/// How the AArch32 handler for a call relates to the AArch64 call of the same meaning.",
        "#[derive(Debug, PartialEq, Eq, Clone, Copy)]",
        "pub enum Layout {",
        "    /// Same arguments and structure layouts: one handler serves both.",
        "    Same,",
        "    /// Register packing or a structure differs with the word size.",
        "    Words,",
        "}",
        "",
        "/// One call: its AArch64 name and number, and the AArch32 call (name and",
        "/// number) the Open Runtime's dispatcher knows it by.",
        "pub struct Call {",
        "    pub name: &'static str,",
        "    pub aarch64: u16,",
        "    pub aarch32_name: &'static str,",
        "    pub aarch32: u16,",
        "    pub layout: Layout,",
        "}",
        "",
        "/// Sorted by AArch64 number.",
        "pub const CALLS: &[Call] = &[",
    ]
    for name, n64, other, n32, layout in rows:
        lines.append(f'    Call {{ name: "{name}", aarch64: {n64}, aarch32_name: "{other}", aarch32: {n32}, layout: Layout::{layout} }},')
    lines += [
        "];",
        "",
        "/// The call an AArch64 program means by `number`, if the table has it.",
        "pub fn aarch64(number: u64) -> Option<&'static Call> {",
        "    CALLS.binary_search_by(|call| u64::from(call.aarch64).cmp(&number)).ok().map(|i| &CALLS[i])",
        "}",
        "",
        "/// The call an AArch32 program means by `number`, if the table has it.",
        "pub fn aarch32(number: u64) -> Option<&'static Call> {",
        "    CALLS.iter().find(|call| u64::from(call.aarch32) == number)",
        "}",
        "",
    ]
    return "\n".join(lines)


def main():
    rows = table()
    if rows is None:
        if "--check" in sys.argv:
            print("gen_linux.py: no Android NDK here, linux.rs not compared with the headers")
            return
        sys.exit("Set ANDROID_NDK_HOME to the Android NDK 28.2.13676358 directory.")
    text = render(rows)
    if "--check" in sys.argv:
        if OUT.read_text() != text:
            sys.exit("runtime/arch/linux.rs is not what the Linux headers give: run python3 runtime/arch/gen_linux.py")
        print(f"runtime/arch/linux.rs matches the Linux headers ({len(rows)} calls)")
        return
    OUT.write_text(text)


if __name__ == "__main__":
    main()
