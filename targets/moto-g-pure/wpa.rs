//! The supplicant calls back synchronously on Radio's sole thread.
use super::{passive_scan::AccessPoint, radio_io, station, wlan_axi::Axi};
use crate::protocol::*;
use core::slice;

extern "C" {
    fn station_wpa_open(
        ctx: *mut core::ffi::c_void,
        mac: *const u8,
        ap: *const u8,
        ssid: *const u8,
        ssid_len: usize,
        beacon: *const u8,
        beacon_len: usize,
        assoc: *const u8,
        assoc_len: usize,
        pmk: *const u8,
    ) -> i32;
    fn station_wpa_rx(frame: *const u8, n: usize) -> i32;
    fn station_wpa_close();
    fn station_wpa_poll();
}

#[no_mangle]
pub(super) unsafe extern "C" fn radio_random(out: *mut u8, n: usize) -> i32 {
    static mut LAST: [u8; 16] = [0; 16];
    for chunk in slice::from_raw_parts_mut(out, n).chunks_mut(16) {
        let r = Message::new(GET, 0x405c, 0).call(7);
        let block: [u8; 16] = r.data[..16].try_into().unwrap();
        if r.op != OK || r.value != 16 || block == [0; 16] || LAST == block {
            return -1;
        }
        LAST = block;
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
    0
}

#[no_mangle]
unsafe extern "C" fn radio_send_eapol(ctx: *mut Axi, p: *const u8, n: usize) -> i32 {
    if (*ctx).transmit_eapol(slice::from_raw_parts(p, n)).is_ok() {
        0
    } else {
        -1
    }
}
#[no_mangle]
unsafe extern "C" fn radio_set_ccmp(
    ctx: *mut Axi,
    pair: i32,
    id: i32,
    key: *const u8,
    seq: *const u8,
    n: usize,
) -> i32 {
    if key.is_null() || n > 8 || (n != 0 && seq.is_null()) || !(0..4).contains(&id) {
        return -1;
    }
    let counter = if n == 0 {
        &[]
    } else {
        slice::from_raw_parts(seq, n)
    };
    if (*ctx)
        .install_ccmp(pair != 0, id as u8, slice::from_raw_parts(key, 16), counter)
        .is_ok()
    {
        0
    } else {
        -1
    }
}

pub fn authorize(axi: &mut Axi, ap: &AccessPoint) -> Result<(), u32> {
    let mut q = Message::new(GET, 0x405d, ap.ssid_len as u32);
    q.put_text(&ap.ssid[..ap.ssid_len]);
    let mut profile = q.call(7);
    if profile.op != OK {
        return Err(50);
    }
    let mac = axi.station;
    let rc = unsafe {
        station_wpa_open(
            (axi as *mut Axi).cast(),
            mac.as_ptr(),
            ap.bssid.as_ptr(),
            ap.ssid.as_ptr(),
            ap.ssid_len,
            ap.rsne.as_ptr(),
            ap.rsne_len,
            station::RSN.as_ptr(),
            station::RSN.len(),
            profile.data.as_ptr(),
        )
    };
    for byte in &mut profile.data {
        unsafe {
            core::ptr::write_volatile(byte, 0);
        }
    }
    radio_io::trace(4, u32::from(rc == 0));
    if rc != 0 {
        return Err(52);
    }
    let start = radio_io::now();
    let mut frame = [0; 1536];
    let result = (|| {
        while !radio_io::expired(start, 8000) {
            unsafe { station_wpa_poll() };
            let n = axi.receive_eapol(&mut frame)?;
            if n != 0 {
                let result = unsafe { station_wpa_rx(frame.as_ptr(), n) };
                radio_io::trace(4, 0x10000 | ((result as u32 & 0xff) << 8) | u32::from(frame[6]));
                match result {
                    1 => return Ok(()),
                    -1 => return Err(52),
                    _ => {}
                }
            }
        }
        Err(52)
    })();
    if result.is_err() {
        unsafe { station_wpa_close() };
    }
    result
}

extern "C" {
    fn station_net_host(mac: *const u8, wait_ms: u32) -> i32;
    fn station_net_relay_password(p: *const u8, n: u32);
    fn station_net_leased() -> i32;
}
pub fn leased() -> bool {
    unsafe { station_net_leased() != 0 }
}
/// Drop the hosted IP network but keep the access point running.
pub unsafe fn close_net() {
    station_net_close();
    LINK_ERROR = false;
}
extern "C" {
    fn ap_wpa_start(radio: *mut core::ffi::c_void, aa: *const u8, spa: *const u8, pmk: *const u8) -> i32;
    fn ap_wpa_rx(frame: *const u8, n: usize) -> i32;
    fn ap_wpa_poll() -> i32;
    fn ap_wpa_done() -> i32;
    fn ap_wpa_report() -> u32;
}
pub fn handshake_report() -> u32 {
    unsafe { ap_wpa_report() }
}
/// Drive the access point's 4-way handshake with the associated phone.
/// A failed handshake waits for the phone to associate again.
pub fn authenticator_step(axi: &mut Axi, pmk: &[u8; 32], eapol: &[u8]) -> Result<(), u32> {
    unsafe {
        if core::mem::take(&mut axi.new_client) {
            let (aa, spa) = (axi.ap.bssid, axi.peer_address());
            ap_wpa_start((axi as *mut Axi).cast(), aa.as_ptr(), spa.as_ptr(), pmk.as_ptr());
        }
        let rc = if eapol.is_empty() { ap_wpa_poll() } else { ap_wpa_rx(eapol.as_ptr(), eapol.len()) };
        if rc < 0 {
            axi.client_state = 0;
        }
    }
    Ok(())
}
pub fn authenticated() -> bool {
    unsafe { ap_wpa_done() != 0 }
}
/// Read the provisioned pairwise master key for `ssid` from the supervisor.
pub fn profile_key(ssid: &[u8]) -> Option<[u8; 32]> {
    let mut q = Message::new(GET, 0x405d, ssid.len() as u32);
    q.put_text(ssid);
    let mut profile = q.call(7);
    if profile.op != OK {
        return None;
    }
    let key = profile.data[..32].try_into().ok();
    for byte in &mut profile.data {
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    key
}
/// Serve the phone's network on the access point and wait for its DHCP lease.
/// The SOCKS password is the hex form of the first eight PMK bytes.
pub unsafe fn host_ip(dev: *mut Axi, pmk: &[u8; 32]) -> bool {
    DEVICE = dev;
    LINK_ERROR = false;
    let mut password = [0u8; 16];
    for (i, b) in pmk[..8].iter().enumerate() {
        password[2 * i] = b"0123456789abcdef"[(b >> 4) as usize];
        password[2 * i + 1] = b"0123456789abcdef"[(b & 15) as usize];
    }
    station_net_relay_password(password.as_ptr(), 16);
    let mac = (*dev).ap.bssid;
    station_net_host(mac.as_ptr(), 0) != 0 && !LINK_ERROR
}
extern "C" {
    fn station_net_close();
    fn station_net_join(mac: *const u8) -> i32;
    fn station_net_poll();
    fn station_net_uuid(out: *mut u8) -> i32;
}
static mut DEVICE: *mut Axi = core::ptr::null_mut();
static mut LINK_ERROR: bool = false;
#[no_mangle]
unsafe extern "C" fn radio_l2_send(p: *const u8, n: usize) -> i32 {
    if DEVICE.is_null() || (*DEVICE).send_ip(slice::from_raw_parts(p, n)).is_err() {
        LINK_ERROR = true;
        return -1;
    }
    0
}
#[no_mangle]
unsafe extern "C" fn radio_l2_recv(p: *mut u8, n: usize) -> i32 {
    if DEVICE.is_null() || LINK_ERROR {
        return -1;
    }
    let mut key = [0; 1536];
    match (*DEVICE).poll_data(slice::from_raw_parts_mut(p, n), &mut key) {
        Ok((size, 0)) => size as i32,
        Ok((_, size)) if (*DEVICE).ap_mode => {
            ap_wpa_rx(key.as_ptr(), size);
            0
        }
        Ok((_, size)) if station_wpa_rx(key.as_ptr(), size) >= 0 => 0,
        _ => {
            LINK_ERROR = true;
            -1
        }
    }
}
#[no_mangle]
extern "C" fn radio_unix_time() -> u32 {
    let r = Message::new(GET, 0x405e, 0).call(7);
    if r.op == OK {
        r.value
    } else {
        0
    }
}
pub unsafe fn join_ip(dev: *mut Axi) -> bool {
    DEVICE = dev;
    LINK_ERROR = false;
    let mac = (*dev).station;
    station_net_join(mac.as_ptr()) != 0 && !LINK_ERROR
}
pub unsafe fn close() {
    station_net_close();
    station_wpa_close();
    DEVICE = core::ptr::null_mut();
    LINK_ERROR = false;
}
pub unsafe fn poll() -> bool {
    station_wpa_poll();
    station_net_poll();
    !LINK_ERROR
}
extern "C" {
    fn station_net_text(host: *const u8, path: *const u8, out: *mut u8, cap: u32) -> i32;
}
/// One line of weather for `CITIES[city]`, cleaned to the bitmap font. Returns its length.
pub unsafe fn weather(city: usize, out: &mut [u8; 40]) -> usize {
    let mut path = [0; 64];
    if crate::protocol::weather_path(city, &mut path).is_none() {
        return 0;
    }
    station_net_text(b"wttr.in\0".as_ptr(), path.as_ptr(), out.as_mut_ptr(), 40).max(0) as usize
}
pub unsafe fn uuid(out: &mut [u8; 36]) -> bool {
    station_net_uuid(out.as_mut_ptr()) == 1
}
