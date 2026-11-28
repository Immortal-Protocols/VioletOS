//! The error numbers the shipped modules name (runtime/open/syscall.rs,
//! which itself is not host-buildable: see mod.rs here).
pub const EPERM: i32 = -1;
pub const ENOENT: i32 = -2;
pub const ESRCH: i32 = -3;
pub const EINTR: i32 = -4;
pub const EIO: i32 = -5;
pub const EBADF: i32 = -9;
pub const ECHILD: i32 = -10;
pub const EAGAIN: i32 = -11;
pub const ENOMEM: i32 = -12;
pub const EACCES: i32 = -13;
pub const EFAULT: i32 = -14;
pub const EEXIST: i32 = -17;
pub const ENODEV: i32 = -19;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EINVAL: i32 = -22;
pub const EMFILE: i32 = -24;
pub const EROFS: i32 = -30;
pub const ENOSYS: i32 = -38;
pub const ENOTEMPTY: i32 = -39;
pub const EADDRINUSE: i32 = -98;
pub const ENETUNREACH: i32 = -101;
pub const ECONNREFUSED: i32 = -111;
pub const EHOSTUNREACH: i32 = -113;

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_TRUNC: u32 = 0o1000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const AT_FDCWD: i32 = -100;
