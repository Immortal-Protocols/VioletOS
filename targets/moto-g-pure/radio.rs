//! Read connectivity-domain power acknowledgements from the always-on SPM.
//! Accessing the WLAN aperture while its bus is isolated can stall the SoC.
use crate::protocol::*;
#[path = "conn_firmware.rs"]
mod conn_firmware;
#[path = "wlan_image.rs"]
mod wlan_image;

#[path = "passive_scan.rs"]
mod passive_scan;
#[path = "radio_io.rs"]
mod radio_io;
mod station;
#[path = "wlan_axi.rs"]
mod wlan_axi;
#[path = "wlan_download.rs"]
mod wlan_download;
#[path = "wmt.rs"]
mod wmt;
mod wpa;
fn record(index: usize, word: u32) {
    unsafe {
        (0x09200000 as *mut u32).add(index).write_volatile(word);
    }
}
fn stage(value: u32) {
    record(10, value);
}
fn memory_request(op: u32) -> u32 {
    let response = Message::new(op, RADIO_MEMORY, 0).call(7);
    let policy = if response.op == OK {
        response.value
    } else {
        0x43
    };
    record(9, policy);
    policy
}
// The supervisor owns the shared power/reset registers. Radio owns clock
// probing, retry timing, and the decision to start or unwind the ROM.
fn start_connectivity() -> Message {
    let domain = Message::new(EXECUTE, 0x4059, 0).call(7);
    if domain.op != OK || domain.value != 1 {
        return domain;
    }
    let mut last = domain;
    last.value = 8;
    for attempt in 0..10 {
        const CLOCK_CHECK: usize = 0x09012000;
        radio_io::wr(CLOCK_CHECK, 0x30000);
        unsafe { delay_ms(1) };
        let clock = radio_io::rd(CLOCK_CHECK);
        last.data[4..8].copy_from_slice(&clock.to_le_bytes());
        if clock & 0x70000 == 0x30000 {
            last = Message::new(EXECUTE, 0x405a, 0).call(7);
            if last.op == OK && last.value == 1 {
                last.value = boot_rom(&mut last.data);
            }
            if last.op != OK || !matches!(last.value, 8 | 9) {
                break;
            }
        } else {
            last.value = 8;
        }
        if attempt < 9 {
            unsafe { delay_ms(20) };
        }
    }
    if last.op != OK || last.value != 11 {
        let cleanup = Message::new(EXECUTE, 0x4054, 0).call(7);
        if cleanup.op != OK || cleanup.value != 1 {
            return cleanup;
        }
    }
    last
}
fn boot_rom(report: &mut [u8; 32]) -> u32 {
    const ROM: usize = 0x09015000;
    const OSC: usize = 0x09013204;
    radio_io::wr(ROM + 0x114, (radio_io::rd(ROM + 0x114) & 0xffff0000) | 0x17);
    radio_io::wr(OSC, radio_io::rd(OSC) & !1);
    radio_io::wr(ROM + 0x150, radio_io::rd(ROM + 0x150) | 0x208001);
    let released = Message::new(EXECUTE, 0x405b, 0).call(7);
    if released.op != OK || released.value != 1 {
        return 14;
    }
    let start = radio_io::now();
    loop {
        let mailbox = radio_io::rd(ROM + 0x600);
        report[8..12].copy_from_slice(&mailbox.to_le_bytes());
        if mailbox == 0x1d1e {
            return 11;
        }
        if radio_io::expired(start, 240) {
            return 14;
        }
    }
}
pub fn run() -> ! {
    let mut scan = passive_scan::Results::default();
    let mut requested = false;
    let mut join_target = None;
    // The supplicant retains its device context for group rekeys across IPC calls.
    static mut DEVICE: wlan_axi::Axi = wlan_axi::Axi::new();
    let dev = core::ptr::addr_of_mut!(DEVICE);
    let mut connected = 0u32;
    let mut usb_job = Message::default();
    let run_test = core::cell::Cell::new(0);
    // Only the antenna profile comes from boot. Read memory policy live.
    let profile = unsafe { (0x095ffff8 as *const u32).read_volatile() };
    let mut memory_policy = memory_request(GET);
    record(14, profile);
    let bundle = unsafe { core::slice::from_raw_parts(0x09400000 as *const u8, 2 << 20) };
    let mut firmware = true;
    let mut probed = false;
    let mut probe_status = 0;
    let active = core::cell::Cell::new(false);
    let mut dispatch = |m: Message| {
        if (m.op, m.key, m.value, m.arg) == (EXECUTE, RADIO, 1, 0) {
            usb_job = Message::new(GET, 0x4060, 0).call(7);
            if usb_job.op != OK || !(1..=32).contains(&usb_job.value) {
                usb_job = Message::default();
            }
            if usb_job.arg != 0 {
                record(64, 0);
                record(65, 0);
                for i in 66..75 {
                    record(i, 0);
                }
                radio_io::barrier();
                record(64, usb_job.arg);
            }
        }
        if connected != 0 {
            if usb_job.arg != 0 {
                if scan.aps[..scan.ap_count].iter().any(|ap| {
                    ap.profile_id() == connected
                        && ap.ssid_len == usb_job.value as usize
                        && ap.ssid[..ap.ssid_len] == usb_job.data[..ap.ssid_len]
                }) {
                    run_test.set(usb_job.arg);
                } else {
                    record(65, 49);
                }
                usb_job = Message::default();
            }
            let alive = unsafe { wpa::poll() };
            if !alive || (m.op == EXECUTE && matches!(m.key, PROFILE | RADIO) && m.value == 0) {
                unsafe { wpa::close() };
                let stopped = unsafe { (*dev).shutdown() }
                    && Message::new(EXECUTE, 0x4054, 0).call(7).value == 1;
                connected = 0;
                active.set(false);
                probed = false;
                probe_status = if stopped { 0 } else { 44 };
                let mut r = if stopped {
                    Message::result(0)
                } else {
                    Message::error(OFFLINE)
                };
                if stopped {
                    r.arg = RADIO_RELEASE_POWER;
                }
                return r;
            }
            return match (m.op, m.key) {
                (GET, PROFILE) | (EXECUTE, PROFILE) if m.op == GET || m.value == connected => {
                    let mut r = Message::result(connected);
                    r.arg = 1;
                    r
                }
                (GET, NETWORK) | (GET, RADIO) | (EXECUTE, RADIO) => {
                    let mut r = Message::result(1);
                    r.arg = RADIO_DIAGNOSTIC;
                    r.put_text(b"WIFI CONNECTED");
                    r
                }
                (EXECUTE, RADIO_TEST) => {
                    let mut id = [0; 36];
                    let mut r = Message::result(2);
                    if unsafe { wpa::uuid(&mut id) } {
                        r = Message::result(1);
                        r.put_text(&id);
                    }
                    r
                }
                (GET, SCAN) => scan_entry(&scan, m.value),
                _ => Message::error(BUSY),
            };
        }
        let joining = m.op == EXECUTE && m.key == PROFILE && m.value != 0;
        if joining && m.arg == 0 {
            if !firmware || !matches!(probe_status, 43 | 46..=54) {
                return Message::error(OFFLINE);
            }
            let mut matches = scan.aps[..scan.ap_count]
                .iter()
                .filter(|ap| ap.profile_id() == m.value);
            let Some(ap) = matches.next().copied() else {
                return Message::error(INVALID);
            };
            if matches.next().is_some() || !ap.wpa2_psk_ccmp {
                return Message::error(INVALID);
            }
            join_target = Some(ap);
            probed = false;
        }
        if (m.op, m.key, m.value, m.arg) == (EXECUTE, RADIO, 1, 0) {
            probed = false;
            join_target = None;
        }
        if firmware && !probed && ((m.op, m.key, m.value) == (EXECUTE, RADIO, 1) || joining) {
            if m.arg == 0 {
                requested = true;
                memory_policy = memory_request(EXECUTE);
                if memory_policy != 1 {
                    probe_status = if memory_policy == 0x31 { 16 } else { 15 };
                    probed = true;
                    unsafe {
                        (0x0920000c as *mut u32).write_volatile(probe_status);
                    }
                    return Message::error(OFFLINE);
                }
                if profile != 1 && profile != 2 {
                    probed = true;
                    probe_status = 16;
                    record(3, 16);
                    return Message::error(OFFLINE);
                }
                let spm = 0x09000000 as *const u32;
                let active = unsafe {
                    (spm.add(0x180 / 4).read_volatile() | spm.add(0x184 / 4).read_volatile())
                        & ((1 << 1) | (1 << 30))
                };
                if active != 0 {
                    probed = true;
                    probe_status = 2;
                    unsafe {
                        (0x0920000c as *mut u32).write_volatile(probe_status);
                    }
                    return Message::error(OFFLINE);
                }
                let emi =
                    unsafe { core::slice::from_raw_parts_mut(0x09800000 as *mut u8, 4 << 20) };
                emi.fill(0);
                unsafe { core::ptr::write_bytes(0x09c00000 as *mut u8, 0, 3 << 20) };
                let staged = conn_firmware::stage(bundle, emi);
                firmware = staged.is_some();
                unsafe {
                    (0x09200014 as *mut u32).write_volatile(staged.unwrap_or(0) as u32);
                }
                if !firmware {
                    probed = true;
                    return Message::error(OFFLINE);
                }
                return Message::new(RADIO_PREPARE_POWER, 0, 0);
            }
            probed = true;
            if m.arg != 1 {
                probe_status = 6;
                unsafe {
                    (0x0920000c as *mut u32).write_volatile(probe_status);
                }
                return Message::error(OFFLINE);
            }
            let result = start_connectivity();
            probe_status = result.value;
            let words = result.text();
            for (i, src) in words[12..].chunks_exact(4).enumerate() {
                record(28 + i, u32::from_le_bytes(src.try_into().unwrap()));
            }
            let report = 0x09200000 as *mut u32;
            unsafe {
                report.add(3).write_volatile(result.value);
                report.add(4).write_volatile(result.arg);
                report
                    .add(8)
                    .write_volatile(u32::from_le_bytes(result.data[8..12].try_into().unwrap()));
                report
                    .add(6)
                    .write_volatile(u32::from_le_bytes(result.data[..4].try_into().unwrap()));
                report
                    .add(7)
                    .write_volatile(u32::from_le_bytes(result.data[4..8].try_into().unwrap()));
            }
            let mut retain = matches!(probe_status, 5 | 10);
            if probe_status == 11 {
                let files = conn_firmware::files(bundle).unwrap();
                unsafe {
                    dev.write(wlan_axi::Axi::new());
                }
                let axi = unsafe { &mut *dev };
                scan = passive_scan::Results::default();
                if let Some(target) = join_target {
                    scan.preferred_ssid = target.ssid;
                    scan.preferred_len = target.ssid_len;
                } else if usb_job.arg != 0 {
                    scan.preferred_ssid = usb_job.data;
                    scan.preferred_len = usb_job.value as usize;
                }
                let result = wmt::enable(files[2], profile, stage)
                    .and_then(|()| axi.scan(files[3], &mut scan, stage))
                    .and_then(|()| {
                        if usb_job.arg != 0 {
                            join_target = scan.aps[..scan.ap_count]
                                .iter()
                                .find(|ap| {
                                    ap.wpa2_psk_ccmp
                                        && ap.ssid_len == usb_job.value as usize
                                        && ap.ssid[..ap.ssid_len] == usb_job.data[..ap.ssid_len]
                                })
                                .copied();
                            if join_target.is_none() {
                                return Err(49);
                            }
                        }
                        if let Some(target) = join_target.take() {
                            let ap = scan.aps[..scan.ap_count]
                                .iter()
                                .find(|ap| {
                                    ap.bssid == target.bssid
                                        && ap.ssid_len == target.ssid_len
                                        && ap.ssid[..ap.ssid_len] == target.ssid[..target.ssid_len]
                                })
                                .ok_or(49u32)?;
                            stage(46);
                            axi.associate(ap)?;
                            stage(52);
                            wpa::authorize(axi, ap)?;
                            stage(53);
                            if !unsafe { wpa::join_ip(dev) } {
                                unsafe { wpa::close() };
                                return Err(53);
                            }
                            connected = ap.profile_id();
                            active.set(true);
                        }
                        Ok(())
                    });
                probe_status = result.err().unwrap_or(43);
                record(11, probe_status);
                for (i, b) in scan.name.iter().copied().enumerate() {
                    unsafe {
                        ((0x09200044 + i) as *mut u8).write_volatile(b);
                    }
                }
                unsafe {
                    core::arch::asm!("dmb sy");
                }
                record(16, scan.name_len as u32);
                record(12, scan.frames);
                record(13, scan.channels as u32);
                if connected != 0 {
                    run_test.set(usb_job.arg);
                    usb_job = Message::default();
                    record(3, 54);
                    let mut r = Message::result(connected);
                    r.arg = RADIO_RETAIN_POWER;
                    return r;
                }
                if axi.shutdown() {
                    let cleanup = Message::new(EXECUTE, 0x4054, 0).call(7);
                    record(15, cleanup.value);
                    retain = cleanup.op != OK || cleanup.value != 1;
                } else {
                    retain = true;
                    record(15, 44);
                }
                if retain && probe_status == 43 {
                    probe_status = 44;
                }
                record(3, probe_status);
            }
            if usb_job.arg != 0 && probe_status != 43 {
                record(65, probe_status);
                usb_job = Message::default();
            }
            let mut reply = if probe_status == 43 {
                Message::result(0)
            } else {
                Message::error(OFFLINE)
            };
            if retain {
                reply.arg = RADIO_RETAIN_POWER;
            }
            return reply;
        }
        if (m.op, m.key) == (GET, SCAN) {
            return scan_entry(&scan, m.value);
        }
        if (m.op, m.key) == (GET, PROFILE) {
            let mut reply = Message::result(0);
            reply.arg = (probe_status == 43) as u32;
            return reply;
        }
        if (m.op, m.key) != (GET, RADIO) {
            return Message::error(OFFLINE);
        }
        if !requested {
            let mut reply = Message::result(0);
            reply.arg = RADIO_DIAGNOSTIC;
            let mut line = *b"WIFI IDLE                ";
            if let Some(release) = option_env!("VIOLET_RADIO_RELEASE") {
                let bytes = release.as_bytes();
                let n = bytes.len().min(15);
                line[10..10 + n].copy_from_slice(&bytes[..n]);
            }
            reply.put_text(&line);
            return reply;
        }
        let base = 0x09000000usize;
        let mask = 1 << 1 | 1 << 30;
        let first = unsafe { ((base + 0x180) as *const u32).read_volatile() } & mask;
        let second = unsafe { ((base + 0x184) as *const u32).read_volatile() } & mask;
        let report = 0x09200000 as *mut u32;
        unsafe {
            report.write_volatile(first);
            report.add(1).write_volatile(second);
            report.add(2).write_volatile(if !firmware {
                2
            } else if report.add(5).read_volatile() != 0 {
                1
            } else {
                0
            });
        }
        let mut reply = Message::result(0);
        reply.arg = RADIO_DIAGNOSTIC
            | (probe_status << 8)
            | ((first | second) != 0) as u32
            | (((first & second) == mask) as u32) << 1;
        if probe_status >= 20 && probe_status != 43 {
            let step = unsafe { report.add(10).read_volatile() };
            let mut line = *b"WIFI STEP 00 ERROR 00";
            line[10] = b'0' + (step / 10 % 10) as u8;
            line[11] = b'0' + (step % 10) as u8;
            line[19] = b'0' + (probe_status / 10 % 10) as u8;
            line[20] = b'0' + (probe_status % 10) as u8;
            reply.put_text(&line);
            return reply;
        }
        if probe_status == 43 {
            if scan.name_len == 0 {
                reply.put_text(if scan.frames == 0 {
                    b"SCAN DONE: NO NETWORKS"
                } else {
                    b"SCAN DONE: HIDDEN SSID"
                });
            } else {
                let mut line = [0; 40];
                line[..6].copy_from_slice(b"FOUND ");
                line[6..6 + scan.name_len].copy_from_slice(&scan.name[..scan.name_len]);
                reply.put_text(&line);
            }
            return reply;
        }
        reply.put_text(if !firmware {
            b"WIFI FIRMWARE MISSING"
        } else if probe_status != 0 || (first == 0 && second == 0) {
            match probe_status {
                11 => b"WIFI CONNECTIVITY ROM UP",
                12 => b"WIFI ROM VERSION MISMATCH",
                13 => b"WIFI MEMORY REMAP FAILED",
                14 => b"WIFI ROM START TIMEOUT",
                15 => match memory_policy {
                    0x43 => b"WIFI MEMORY SERVICE UNAVAILABLE",
                    0x40..=0x42 => b"WIFI MPU READ FAILED",
                    0x10 => b"WIFI REGION 25 IN USE",
                    0x11 => b"WIFI REGION 29 IN USE",
                    0x20 => b"WIFI REGION 25 SET FAIL",
                    0x21 => b"WIFI REGION 29 SET FAIL",
                    0x30 => b"WIFI DOMAIN ALREADY ON",
                    1 => b"WIFI SUPERVISOR POLICY",
                    _ => b"WIFI POLICY UNKNOWN",
                },
                16 => b"WIFI UNKNOWN RF PROFILE",
                1 => b"WIFI CHIP 10020300",
                7 => b"WIFI BUS PROTECTION",
                8 => b"WIFI CLOCK NOT READY",
                9 => b"WIFI UNEXPECTED CHIP ID",
                10 => b"WIFI BUS RESTORE FAILED",
                2 => b"WIFI POWER STATE SKIPPED",
                3 => b"WIFI POWER CONTROL LOCKED",
                4 => b"WIFI POWER ON TIMEOUT",
                6 => b"WIFI RAIL START FAILED",
                5 => b"WIFI POWER OFF TIMEOUT",
                _ => b"WIFI DOMAIN OFF",
            }
        } else if first == mask && second == mask {
            b"WIFI DOMAIN ON"
        } else {
            b"WIFI DOMAIN PARTIAL"
        });
        reply
    };
    extern "C" {
        fn ipc_try_receive(slot: u32, m: *mut Message) -> u32;
    }
    loop {
        let mut m = Message::default();
        if active.get() {
            if run_test.replace(0) != 0 {
                let mut uuid = [0; 36];
                let ok = unsafe { wpa::uuid(&mut uuid) };
                for (i, bytes) in uuid.chunks_exact(4).enumerate() {
                    record(66 + i, u32::from_le_bytes(bytes.try_into().unwrap()));
                }
                radio_io::barrier();
                record(65, if ok { 1 } else { 55 });
            }
            if unsafe { ipc_try_receive(SERVER, &mut m) } == 0 {
                unsafe {
                    wpa::poll();
                    delay_ms(1);
                }
                continue;
            }
        } else {
            m = Message::receive(SERVER).1;
        }
        let r = dispatch(m);
        if m.op == EXECUTE && m.key == RADIO && r.op == OFFLINE {
            let state = unsafe { ((0x09200000 as *const u32).add(3)).read_volatile() };
            record(65, state.max(2));
        }
        r.reply();
    }
}

fn scan_entry(scan: &passive_scan::Results, index: u32) -> Message {
    let mut reply = Message::result(0);
    if let Some(ap) = scan.aps[..scan.ap_count].get(index as usize) {
        // Persist an identifier for the AP, never its position in a scan.
        reply.value = ap.profile_id();
        reply.arg = ap.channel as u32;
        reply.put_text(&ap.ssid[..ap.ssid_len]);
    }
    reply
}
