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
#[path = "softap.rs"]
mod softap;
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
    // A pairing code from Home's Phone Internet page, kept until hosting starts or fails.
    let mut relay_job: Option<[u8; RELAY_CODE_LEN]> = None;
    let relay_failure = core::cell::Cell::new(0u32);
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
    // The access point of the current station connection, to rejoin when the link fails.
    let joined: core::cell::Cell<Option<passive_scan::AccessPoint>> = core::cell::Cell::new(None);
    let mut dispatch = |m: Message| {
        if (m.op, m.key, m.value) == (EXECUTE, RELAY, 2) {
            let mut bytes = [0u8; RELAY_CODE_LEN / 2];
            if unsafe { wpa::radio_random(bytes.as_mut_ptr(), bytes.len()) } != 0 {
                return Message::error(OFFLINE);
            }
            let mut code = [0u8; RELAY_CODE_LEN];
            for (i, b) in bytes.iter().enumerate() {
                code[2 * i] = b"0123456789ABCDEF"[(b >> 4) as usize];
                code[2 * i + 1] = b"0123456789ABCDEF"[(b & 15) as usize];
            }
            let mut r = Message::result(1);
            r.put_text(&code);
            return r;
        }
        let relay_request = (m.op, m.key, m.value) == (EXECUTE, RELAY, 1);
        if relay_request {
            let text = m.text();
            if !valid_relay_code(&text) {
                return Message::error(INVALID);
            }
            if connected != 0 {
                // Already hosting (or joined to a network): report the current state.
                let mut r = Message::result(relay_state());
                r.arg = relay_failure.get();
                return r;
            }
            relay_job = Some(text[..RELAY_CODE_LEN].try_into().unwrap());
            if m.arg == 0 {
                relay_failure.set(0);
                probed = false;
                join_target = None;
            }
        }
        if (m.op, m.key) == (GET, RELAY) && connected == 0 {
            let mut r = Message::result(RELAY_OFF);
            r.arg = relay_failure.get();
            return r;
        }
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
            // A USB join of an ordinary network while hosting the relay: stop hosting
            // first (the next join request then joins it).
            let relaying = unsafe { (*core::ptr::addr_of!(RELAY_AP)).is_some() };
            let usb_ssid = &usb_job.data[..(usb_job.value as usize).min(32)];
            let leave_relay = usb_job.arg != 0 && relaying && !usb_ssid.starts_with(b"VioletRelay");
            if leave_relay {
                record(65, 60);
                usb_job = Message::default();
            } else if usb_job.arg != 0 && relaying {
                // Relay hosting: any join request re-runs the HTTPS test through the phone.
                run_test.set(usb_job.arg);
                unsafe {
                    if let Some(r) = (*core::ptr::addr_of_mut!(RELAY_AP)).as_mut() {
                        r.started = radio_io::now();
                    }
                }
                usb_job = Message::default();
            } else if usb_job.arg != 0 {
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
            if !alive || leave_relay || (m.op == EXECUTE && matches!(m.key, PROFILE | RADIO | RELAY) && m.value == 0) {
                unsafe { wpa::close() };
                unsafe { RELAY_AP = None };
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
                    r.put_text(match relay_state() {
                        RELAY_OFF => b"WIFI CONNECTED" as &[u8],
                        RELAY_ONLINE => b"PHONE INTERNET ON",
                        RELAY_JOINED => b"PHONE JOINED",
                        _ => b"WAITING FOR PHONE",
                    });
                    r
                }
                (GET, RELAY) => {
                    let mut r = Message::result(relay_state());
                    r.arg = relay_failure.get();
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
        if firmware && !probed && ((m.op, m.key, m.value) == (EXECUTE, RADIO, 1) || joining || relay_request) {
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
                let mut app_name = [0u8; 32];
                let usb_name = &usb_job.data[..(usb_job.value as usize).min(32)];
                // Home's pairing code wins over a USB join job; both host a relay network.
                let pairing = relay_job.take().map(|code| {
                    let n = relay_ssid(&code, &mut app_name);
                    let mut password = [0u8; 16];
                    password[..8].copy_from_slice(&code[4..]);
                    (n, Some(code), password)
                });
                let relay = pairing.is_some()
                    || (usb_job.arg != 0 && usb_name.starts_with(b"VioletRelay"));
                if relay {
                    let result = wmt::enable(files[2], profile, stage).and_then(|()| match pairing {
                        Some((n, _, password)) => relay_start(axi, &app_name[..n], files[3], None, &password[..8]),
                        None => {
                            let pmk = wpa::profile_key(usb_name).ok_or(50u32)?;
                            let mut password = [0u8; 16];
                            for (i, b) in pmk[..8].iter().enumerate() {
                                password[2 * i] = b"0123456789abcdef"[(b >> 4) as usize];
                                password[2 * i + 1] = b"0123456789abcdef"[(b & 15) as usize];
                            }
                            relay_start(axi, usb_name, files[3], Some(pmk), &password)
                        }
                    });
                    if pairing.is_some() {
                        relay_failure.set(result.err().unwrap_or(0));
                    }
                    probe_status = result.err().unwrap_or(43);
                    record(11, probe_status);
                    record(12, axi.mgmt_frames);
                    if probe_status == 43 {
                        connected = 0x5245_4c59;
                        active.set(true);
                        run_test.set(usb_job.arg);
                        usb_job = Message::default();
                        record(3, 54);
                        let mut r = Message::result(connected);
                        r.arg = RADIO_RETAIN_POWER;
                        return r;
                    }
                    unsafe { wpa::close() };
                    let stopped = axi.shutdown()
                        && Message::new(EXECUTE, 0x4054, 0).call(7).value == 1;
                    record(15, if stopped { 0 } else { 44 });
                    if usb_job.arg != 0 {
                        record(65, probe_status);
                        usb_job = Message::default();
                    }
                    let mut reply = Message::error(OFFLINE);
                    if !stopped {
                        reply.arg = RADIO_RETAIN_POWER;
                    }
                    return reply;
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
                            // Every access point with this name is a candidate, 2.4 GHz first:
                            // one that does not answer authentication should not end the join.
                            let mut order = [0usize; 16];
                            let mut count = 0;
                            for pass in 0..2 {
                                for (k, ap) in scan.aps[..scan.ap_count].iter().enumerate() {
                                    let same = ap.wpa2_psk_ccmp && ap.ssid_len == target.ssid_len
                                        && ap.ssid[..ap.ssid_len] == target.ssid[..target.ssid_len];
                                    if same && ((ap.channel <= 14) == (pass == 0)) && count < order.len() {
                                        order[count] = k;
                                        count += 1;
                                    }
                                }
                            }
                            if count == 0 {
                                return Err(49);
                            }
                            stage(46);
                            let mut chosen = None;
                            let mut last = 47;
                            let mut tried = 0u32;
                            for &k in order[..count].iter() {
                                tried = tried << 8 | u32::from(scan.aps[k].channel);
                                match axi.associate(&scan.aps[k]) {
                                    Ok(_) => {
                                        chosen = Some(k);
                                        break;
                                    }
                                    Err(e) => last = e,
                                }
                            }
                            // moto-debug's "wlan_result" after a join: the channels tried, last in the low byte.
                            record(42, tried);
                            let ap = &scan.aps[chosen.ok_or(last)?];
                            stage(52);
                            wpa::authorize(axi, ap)?;
                            stage(53);
                            if !unsafe { wpa::join_ip(dev) } {
                                unsafe { wpa::close() };
                                return Err(53);
                            }
                            connected = ap.profile_id();
                            joined.set(Some(*ap));
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
        // Open apps' TCP sockets (ip/sockets.c), in pages shared with the Open Runtime.
        fn open_sockets_poll(base: *mut u8, online: u32);
    }
    const SOCKETS: *mut u8 = 0x0b00_0000 as *mut u8;
    loop {
        let mut m = Message::default();
        wpa::tick_clock();
        // 1 on a joined network, 2 through the phone's relay once it reaches the internet.
        let online = if !active.get() {
            0
        } else if let Some(relay) = unsafe { (*core::ptr::addr_of!(RELAY_AP)).as_ref() } {
            if relay.online { 2 } else { 0 }
        } else {
            1
        };
        unsafe { open_sockets_poll(SOCKETS, online) };
        if active.get() {
            let relaying = unsafe { (*core::ptr::addr_of!(RELAY_AP)).is_some() };
            let leased = relaying && relay_step(dev);
            if relaying && !leased && run_test.get() != 0 {
                let started = unsafe { (*core::ptr::addr_of!(RELAY_AP)).as_ref().map_or(0, |r| r.started) };
                if radio_io::expired(started, 150_000) {
                    run_test.set(0);
                    record(65, 59);
                }
            }
            let job = if !relaying || leased { run_test.replace(0) } else { 0 };
            if job != 0 {
                let mut uuid = [0; 36];
                let ok = unsafe { wpa::uuid(&mut uuid) };
                for (i, bytes) in uuid.chunks_exact(4).enumerate() {
                    record(66 + i, u32::from_le_bytes(bytes.try_into().unwrap()));
                }
                radio_io::barrier();
                record(65, if ok { 1 } else { 55 });
            }
            if unsafe { ipc_try_receive(SERVER, &mut m) } == 0 {
                let alive = unsafe { wpa::poll() };
                // A station link that failed (deauthentication or a driver error):
                // associate again, redo the handshake and DHCP, with the firmware
                // still running. Apps' connections reset and they reconnect.
                if !alive && !relaying {
                    if let Some(ap) = joined.get() {
                        for attempt in 0..3 {
                            stage(70 + attempt);
                            if unsafe { relink(dev, &ap) } {
                                stage(73);
                                break;
                            }
                            unsafe { delay_ms(500) };
                        }
                    }
                }
                unsafe { delay_ms(1) };
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

/// Re-associate with `ap` and redo the WPA2 handshake and DHCP, keeping the
/// firmware running. Used when a station link fails.
unsafe fn relink(dev: *mut wlan_axi::Axi, ap: &passive_scan::AccessPoint) -> bool {
    wpa::close();
    let axi = &mut *dev;
    axi.associate(ap).is_ok() && wpa::authorize(axi, ap).is_ok() && wpa::join_ip(dev)
}

/// Relay access point that stays up between requests while the phone comes and goes.
struct Relay {
    pmk: [u8; 32],
    password: [u8; 16],
    password_len: usize,
    wpa2: bool,
    net_up: bool,
    /// The phone answered a time request through its relay, so internet works.
    online: bool,
    last_time_try: u32,
    started: u32,
}
static mut RELAY_AP: Option<Relay> = None;

/// Host `ssid` as an access point. Names starting with "VioletRelayOpen" are open
/// networks; others use WPA2-PSK. The phone is served from Radio's event loop.
/// `pmk` is the USB-provisioned key (WPA2 networks need it); `password` is what the
/// phone's SOCKS relay expects.
fn relay_start(
    axi: &mut wlan_axi::Axi,
    ssid: &[u8],
    image: &[u8],
    pmk: Option<[u8; 32]>,
    password: &[u8],
) -> Result<(), u32> {
    let wpa2 = !ssid.starts_with(b"VioletRelayOpen");
    if wpa2 && pmk.is_none() {
        return Err(50);
    }
    axi.boot(image, stage)?;
    stage(56);
    axi.start_ap(ssid, 6, wpa2)?;
    stage(58);
    let mut stored = [0u8; 16];
    let password_len = password.len().min(16);
    stored[..password_len].copy_from_slice(&password[..password_len]);
    unsafe {
        RELAY_AP = Some(Relay {
            pmk: pmk.unwrap_or([0; 32]),
            password: stored,
            password_len,
            wpa2,
            net_up: false,
            online: false,
            last_time_try: 0,
            started: radio_io::now(),
        })
    };
    Ok(())
}

/// RELAY_OFF when not hosting, otherwise how far the phone has got.
fn relay_state() -> u32 {
    match unsafe { (*core::ptr::addr_of!(RELAY_AP)).as_ref() } {
        None => RELAY_OFF,
        Some(r) if r.online => RELAY_ONLINE,
        Some(r) if r.net_up && wpa::leased() => RELAY_JOINED,
        Some(_) => RELAY_WAITING,
    }
}

/// Service the access point once. Returns true when the phone holds a DHCP lease.
fn relay_step(dev: *mut wlan_axi::Axi) -> bool {
    let Some(relay) = (unsafe { (*core::ptr::addr_of_mut!(RELAY_AP)).as_mut() }) else { return false };
    let axi = unsafe { &mut *dev };
    let ready = axi.client_state == 2 && !axi.new_client && (!relay.wpa2 || wpa::authenticated());
    if relay.net_up && !ready {
        // The phone left or is associating again: drop IP state and wait for it.
        unsafe { wpa::close_net() };
        relay.net_up = false;
        relay.online = false;
        stage(58);
    }
    if !relay.net_up {
        if !relay.wpa2 {
            axi.new_client = false; // open network: association alone admits the phone
        }
        let mut frame = [0; 1518];
        let mut key = [0; 1536];
        for _ in 0..8 {
            let Ok((_, eapol)) = axi.ap_poll(&mut frame, &mut key) else { break };
            if relay.wpa2 && axi.client_state == 2 {
                let _ = wpa::authenticator_step(axi, &relay.pmk, &key[..eapol]);
            }
            if eapol == 0 {
                break;
            }
        }
        record(12, axi.mgmt_frames | axi.rx_mgmt << 16);
        record(13, wpa::handshake_report());
        if !relay.wpa2 {
            axi.new_client = false;
        }
        let ready = axi.client_state == 2 && !axi.new_client && (!relay.wpa2 || wpa::authenticated());
        if ready {
            stage(61);
            relay.net_up = unsafe { wpa::host_ip(dev, &relay.password[..relay.password_len]) };
        }
        return false;
    }
    unsafe { wpa::poll() };
    let leased = wpa::leased();
    if leased {
        stage(63);
        // Ask the phone for the time (every 5 s until it answers). TLS needs it, and an
        // answer proves the relay reaches the internet side of the phone.
        if !relay.online && (relay.last_time_try == 0 || radio_io::expired(relay.last_time_try, 5000)) {
            relay.last_time_try = radio_io::now() | 1;
            relay.online = unsafe { wpa::phone_time() };
        }
    } else {
        relay.online = false;
    }
    leased
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
