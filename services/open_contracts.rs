//! Generalized, app-agnostic service contracts for Display, Audio, Radio, and Storage.
//!
//! Moving to these 4 raw interfaces removes all app-specific logic from OS
//! services: `minimp3` and album playback leave `services/audio`, `MUSIC_HOST`
//! and `wttr.in` leave `services/radio`, and 4x4 KiB fixed slots leave `services/storage`.

use crate::runtime_open_abi::{AudioConfig, PresentRect, SurfaceInfo};

pub type ServiceErr = i32;

/// `services/display`: zero-copy 720x1600 surfaces (generalizing PR #13's `HD_PRESENT`).
pub trait DisplayService {
    /// Allocate a double-buffered surface and return its frame caps for mapping
    /// into both Display (read-only) and the owning app (read-write).
    fn alloc_surface(&mut self, app_id: u32) -> Result<(u32, SurfaceInfo), ServiceErr>;
    /// Copy the dirty rectangle from `rect.buffer_index` to scanout if focused.
    fn present(&mut self, surface_id: u32, rect: PresentRect) -> Result<(), ServiceErr>;
    fn set_focus(&mut self, surface_id: u32);
    fn free_surface(&mut self, surface_id: u32);
}

/// `services/audio`: pure multi-app PCM mixer + hardware volume/jack driver.
/// Apps decode MP3/Opus/FLAC themselves and stream raw 48 kHz samples here.
pub trait AudioService {
    /// Open a new PCM stream backed by a shared-memory ring (`AudioRingHeader`).
    fn open_stream(&mut self, app_id: u32, cfg: AudioConfig) -> Result<u32, ServiceErr>;
    /// Notify Audio that new PCM bytes were appended to the ring.
    fn notify_written(&mut self, stream_id: u32);
    fn close_stream(&mut self, stream_id: u32);
    fn set_volume(&mut self, level: u32);
}

/// `services/radio`: open Wi-Fi + TCP/UDP/DNS sockets over lwIP (no URL allowlists).
pub trait RadioNetService {
    fn socket_open(&mut self, app_id: u32, tcp: bool) -> Result<u32, ServiceErr>;
    fn dns_resolve(&mut self, hostname: &str) -> Result<[u8; 4], ServiceErr>;
    fn connect(&mut self, sock_id: u32, ip: [u8; 4], port: u16) -> Result<(), ServiceErr>;
    fn bind(&mut self, sock_id: u32, ip: [u8; 4], port: u16) -> Result<(), ServiceErr>;
    fn send(&mut self, sock_id: u32, data: &[u8]) -> Result<usize, ServiceErr>;
    fn recv(&mut self, sock_id: u32, buf: &mut [u8]) -> Result<usize, ServiceErr>;
    fn close(&mut self, sock_id: u32);
}

/// `services/storage`: real hierarchical filesystem (`/apps`, `/data`, `/shared`)
/// backed by eMMC (Moto) or a larger raw disk image (QEMU).
pub trait StorageFsService {
    fn open(&mut self, path: &str, flags: u32) -> Result<u32, ServiceErr>;
    fn read(&mut self, handle: u32, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceErr>;
    fn write(&mut self, handle: u32, offset: u64, data: &[u8]) -> Result<usize, ServiceErr>;
    fn readdir(&mut self, handle: u32, out_names: &mut [u8]) -> Result<usize, ServiceErr>;
    fn mkdir(&mut self, path: &str) -> Result<(), ServiceErr>;
    fn unlink(&mut self, path: &str) -> Result<(), ServiceErr>;
    fn close(&mut self, handle: u32);
}
