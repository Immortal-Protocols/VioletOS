//! Language-agnostic ABI for `/dev/violet/*` virtual devices.
//!
//! Any language that can open files, `mmap` memory, and read/write structs
//! (Python, Go, Rust, C, Zig, ...) uses these exact layouts directly—no
//! VioletOS-specific FFI or SDK required.

pub const DEV_SCREEN: &str = "/dev/violet/screen";
pub const DEV_AUDIO: &str = "/dev/violet/audio";
pub const DEV_EVENTS: &str = "/dev/violet/events";
pub const DEV_SYS: &str = "/dev/violet/sys";
pub const DEV_CAMERA: &str = "/dev/violet/camera";
/// The microphone: `read` returns 16 kHz s16le mono samples as they arrive.
pub const DEV_MIC: &str = "/dev/violet/mic";

// ---------- /dev/violet/screen ----------
//
// 1. open("/dev/violet/screen", O_RDWR) -> allocates a private 720x1600 double buffer.
// 2. ioctl(fd, SCREEN_IOC_INFO, &SurfaceInfo) -> learn width, height, stride, format.
// 3. mmap(NULL, stride * height * buffers, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0)
// 4. Draw directly into the mapped BGRA8888 pixels.
// 5. write(fd, &PresentRect, sizeof(PresentRect)) -> the Open Runtime copies the
//    rectangle to scanout when the panel is not reading those rows (no tearing);
//    the write returns once it is on its way to the panel.

pub const SCREEN_IOC_INFO: u32 = 0x5601;
/// Block until the panel starts its next frame; fills a VsyncInfo if the
/// argument is not NULL. Presents are tear-free whether or not an app waits.
pub const SCREEN_IOC_WAIT_VSYNC: u32 = 0x5604;
pub const PIXEL_FORMAT_BGRA8888: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SurfaceInfo {
    pub width: u32,        // 720
    pub height: u32,       // 1600
    pub stride_bytes: u32, // 720 * 4
    pub format: u32,       // PIXEL_FORMAT_BGRA8888
    pub buffers: u32,      // 2 (front/back)
    pub buffer_bytes: u32, // stride_bytes * height (page-aligned)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VsyncInfo {
    pub seq: u32,          // frames since start
    pub period_us: u32,    // measured frame period (16,7xx on the handset)
    pub timestamp_us: u64, // when this frame began, CLOCK_MONOTONIC
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PresentRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub buffer_index: u32, // 0 or 1
    pub flags: u32,        // bit 0 = wait for vsync
}

// ---------- /dev/violet/audio ----------
//
// Apps can either:
//   (a) Simple path: open("/dev/violet/audio", O_WRONLY) and write() 48kHz stereo s16le PCM.
//   (b) Zero-copy path: mmap() the shared AudioRingHeader + PCM ring buffer directly.
// Audio service mixes all open streams and applies hardware volume & headphone routing.

pub const AUDIO_IOC_CONFIG: u32 = 0x5610;
pub const AUDIO_FMT_S16LE: u32 = 1;
pub const AUDIO_FMT_F32LE: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioConfig {
    pub sample_rate: u32,   // default 48_000
    pub channels: u32,      // default 2 (stereo)
    pub format: u32,        // AUDIO_FMT_S16LE
    pub keep_in_bg: u32,    // 1 = keep streaming after app leaves foreground
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioRingHeader {
    pub magic: u32,         // 0x41554431 ("AUD1")
    pub capacity_bytes: u32,
    pub write_pos: u32,     // written by app (monotonic byte counter)
    pub read_pos: u32,      // written by Audio service
    pub underruns: u32,
    pub sample_rate: u32,
}

// ---------- /dev/violet/camera ----------
//
// The rear camera, for the app in front.
// 1. open("/dev/violet/camera", O_RDONLY) starts the sensor (ENODEV without one).
// 2. read(fd, buf, len) returns a CameraFrame and then the newest frame's
//    pixels (up to len - 32 bytes), once per new frame. It blocks (or, with
//    O_NONBLOCK, returns EAGAIN) until there is one, and while the app is not
//    in front. poll() reports POLLIN when a new frame is ready.
// 3. close(fd), or leaving the front, turns the camera off.
// Frames are raw Bayer as the sensor delivers them: demosaic them yourself.
// The Moto G Pure's Hi-1336 gives 1280 x 720 RAW8 at 30 frames per second.

pub const CAMERA_MAGIC: u32 = 0x3146_4356; // "VCF1"
pub const CAMERA_RAW8: u32 = 1; // one byte per pixel
pub const CAMERA_RAW10: u32 = 2; // MIPI RAW10: four high bytes, then their low bits
pub const CAMERA_STREAMING: u32 = 2;
pub const CAMERA_FAILED: u32 = 3;
pub const CAMERA_TORN: u32 = 1 << 31;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CameraFrame {
    pub magic: u32,    // CAMERA_MAGIC
    pub sequence: u32, // frames since the camera started
    pub width: u32,
    pub height: u32,
    pub stride: u32,   // bytes per row
    pub format: u32,   // low byte CAMERA_RAW8/RAW10; next byte the Bayer order of the
                       // top-left 2 x 2: 0 RGGB, 1 GRBG, 2 GBRG, 3 BGGR
    pub sensor: u32,   // the sensor's ID, e.g. 0x1336
    pub state: u32,    // low byte CAMERA_STREAMING or CAMERA_FAILED, error code
                       // above it; CAMERA_TORN if the frame changed while copied
}

// ---------- /dev/violet/events ----------
//
// Readable stream of fixed 24-byte events; works with poll()/epoll() in every language.

pub const EVENT_TOUCH_DOWN: u16 = 1;
pub const EVENT_TOUCH_MOVE: u16 = 2;
pub const EVENT_TOUCH_UP: u16 = 3;
pub const EVENT_KEY_DOWN: u16 = 4;   // code = KEY_VOL_UP / KEY_VOL_DOWN / KEY_POWER
pub const EVENT_KEY_UP: u16 = 5;
pub const EVENT_JACK: u16 = 6;       // value = 0 unplugged, 1 headphones
pub const EVENT_LIFECYCLE: u16 = 7;  // value = FOCUS_GAINED / FOCUS_LOST / SLEEP / QUIT

pub const KEY_VOL_UP: u16 = 1;
pub const KEY_VOL_DOWN: u16 = 2;
pub const KEY_HOME: u16 = 3;
/// Sent to Home alone, with value 1, when the power key is held: it shows the
/// power sheet. Short presses turn the screen off and on and reach no app.
pub const KEY_POWER: u16 = 4;

pub const LIFECYCLE_FOCUS_GAINED: i32 = 1;
pub const LIFECYCLE_FOCUS_LOST: i32 = 2;
pub const LIFECYCLE_QUIT: i32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEvent {
    pub timestamp_us: u64,
    pub kind: u16,
    pub code: u16,
    pub x: i32,
    pub y: i32,
    pub value: i32,
}

// ---------- /dev/violet/sys ----------
//
// Line-oriented text control socket/file for system queries and actions:
//   write "status\n"        -> read "battery_mv=4120 wifi=up volume=10 headphones=0\n"
//   write "volume 12\n"     -> sets hardware volume 0..15
//   write "launch /apps/music/main.py\n" -> opens another app
//   write "wakelock 1\n"    -> keeps CPU awake while streaming in background
//   write "lock\n"          -> read "locked=0 set=1 wait=0\n" (the screen lock: runtime/open/power.rs)
