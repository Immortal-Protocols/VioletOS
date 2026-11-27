//! Host tests for 802.11w on the handset's Wi-Fi station: what a scan agrees
//! with an access point, the RSN element sent to it, the protected-frame
//! checks (in pmf.rs), and Radio's driver (wlan_axi.rs) against a register
//! model of the firmware. No firmware bundle needed: `make test-unit` runs it.
//! `make test-wpa` runs it again on hostap's AES, the handset's cipher.
#![allow(dead_code)]
mod passive_scan;
mod pmf;
mod sae;
#[path = "test_radio_bus.rs"]
mod radio_io;
mod softap;
mod station;
mod wlan_axi;
/// The firmware download is not under test here (test_radio.rs has it, with
/// the vendor's image): the driver starts from `Axi::started`.
mod wlan_download {
    pub trait Transport {
        type Error;
        fn command(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
        fn command_result(&mut self) -> Result<&[u8], Self::Error>;
        fn data(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
    }
    pub enum Error<E> {
        Image,
        Bus(E),
    }
    pub fn upload<T: Transport>(_: &[u8], _: &mut u8, _: &mut T) -> Result<(), Error<T::Error>> {
        Err(Error::Image)
    }
}

fn hex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    s.chunks(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect()
}

/// The access point `scanned`, as a build with VIOLET_WIFI_PMF_WHEN_OFFERED keeps it.
fn offered(rsn_tail: &str) -> passive_scan::AccessPoint {
    let mut ap = scanned(rsn_tail, true).unwrap();
    let body = [hex("0100 000fac04 0100 000fac04"), hex(rsn_tail)].concat();
    (ap.wpa2_psk_ccmp, ap.akm, ap.mfp) = passive_scan::personal(&body, true).unwrap();
    ap
}

/// Scan one beacon whose RSN element body is `rsn` (after version, group and
/// pairwise CCMP): the access point as the scan keeps it.
fn scanned(rsn_tail: &str, privacy: bool) -> Option<passive_scan::AccessPoint> {
    let mut frame = vec![0u8; 36];
    frame[0] = 0x80;
    frame[16..22].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
    frame[34] = if privacy { 0x11 } else { 0x01 };
    frame.extend_from_slice(b"\x00\x04test\x01\x03\x8c\x98\xb0\x03\x01\x06");
    let body = [hex("0100 000fac04 0100 000fac04"), hex(rsn_tail)].concat();
    frame.extend_from_slice(&[48, body.len() as u8]);
    frame.extend_from_slice(&body);
    let mut results = passive_scan::Results::default();
    results.frame(&frame);
    (results.ap_count == 1).then(|| results.aps[0])
}

const PSK: &str = "0100 000fac02";
const PSK_SHA256: &str = "0100 000fac06";
const BOTH: &str = "0200 000fac02 000fac06";
/// WPA2/WPA3 transition mode: PSK and SAE.
const TRANSITION: &str = "0200 000fac02 000fac08";
const SAE: &str = "0100 000fac08";

#[test]
fn scan_agrees_key_management_and_frame_protection() {
    assert!(!passive_scan::PMF_WHEN_OFFERED, "tests run on the shipped default");
    // (authentication suites and what follows them, joinable; then key
    // management and 802.11w as shipped, and with VIOLET_WIFI_PMF_WHEN_OFFERED)
    for (tail, joinable, shipped, when_offered) in [
        // No capabilities field, or none of the 802.11w bits: as before.
        (PSK.to_string(), true, (2, 0), (2, 0)),
        (format!("{PSK} 0000"), true, (2, 0), (2, 0)),
        (format!("{PSK} 0c00"), true, (2, 0), (2, 0)),
        // Capable (bit 7): protection is negotiated only when asked for at
        // build time. Required (bit 6 too): always.
        (format!("{PSK} 8000"), true, (2, 0), (2, 1)),
        (format!("{PSK} c000"), true, (2, 2), (2, 2)),
        (format!("{PSK} 8c00"), true, (2, 0), (2, 1)),
        // SHA-256 key management is preferred when frames are protected.
        (format!("{BOTH} 8000"), true, (2, 0), (6, 1)),
        (format!("{BOTH} c000"), true, (6, 2), (6, 2)),
        (format!("{BOTH} 0000"), true, (2, 0), (2, 0)),
        (format!("{PSK_SHA256} c000"), true, (6, 2), (6, 2)),
        (format!("{PSK_SHA256} 8000"), true, (6, 0), (6, 1)),
        (format!("{PSK_SHA256} 0000"), true, (6, 0), (6, 0)),
        // Transition mode is joined with the passphrase.
        (format!("{TRANSITION} 8000"), true, (2, 0), (2, 1)),
        // WPA3-only, Enterprise: not for a passphrase.
        (format!("{SAE} c000"), false, (0, 0), (0, 0)),
        ("0100 000fac01 8000".to_string(), false, (0, 0), (0, 0)),
        // Required without capable is not a valid offer.
        (format!("{PSK} 4000"), false, (0, 0), (0, 0)),
        // The group management cipher: BIP-CMAC-128 named explicitly, after
        // no PMKIDs or one.
        (format!("{PSK} c000 0000 000fac06"), true, (2, 2), (2, 2)),
        (format!("{PSK} 8000 0100 {} 000fac06", "11".repeat(16)), true, (2, 0), (2, 1)),
        (format!("{PSK} 8000 0000"), true, (2, 0), (2, 1)),
        // Another one (BIP-GMAC-256): join unprotected if allowed, else not at all.
        (format!("{PSK} 8000 0000 000fac0c"), true, (2, 0), (2, 0)),
        (format!("{PSK} c000 0000 000fac0c"), false, (0, 0), (0, 0)),
    ] {
        let ap = scanned(&tail, true).unwrap_or_else(|| panic!("{tail}: dropped"));
        assert_eq!((ap.wpa2_psk_ccmp, (ap.akm, ap.mfp)), (joinable, shipped), "{tail}");
        assert!(!ap.open);
        let ap = offered(&tail);
        assert_eq!((ap.wpa2_psk_ccmp, (ap.akm, ap.mfp)), (joinable, when_offered), "{tail} when offered");
    }
    // Malformed elements drop the beacon, as before.
    for tail in [format!("{PSK} 80"), format!("{PSK} 8000 01"), format!("{PSK} 8000 0100 1111"), format!("{PSK} 8000 0000 000fac")] {
        assert!(scanned(&tail, true).is_none(), "{tail}");
    }
    // An RSN element without the privacy bit is not joinable and agrees nothing.
    let ap = scanned(&format!("{BOTH} c000"), false).unwrap();
    assert_eq!((ap.wpa2_psk_ccmp, ap.akm, ap.mfp, ap.open), (false, 0, 0, false));
}

#[test]
fn rsn_element_matches_what_was_agreed() {
    let element = |tail: &str| {
        let ap = offered(tail);
        let mut out = [0; station::RSN_MAX];
        let n = station::rsne(&ap, &mut out);
        (ap, out[..n].to_vec())
    };
    // Without 802.11w the element is byte for byte what Radio always sent.
    let (_, plain) = element(&format!("{PSK} 0000"));
    assert_eq!(plain, station::RSN);
    assert_eq!(plain, hex("3014 0100 000fac04 0100 000fac04 0100 000fac02 0000"));
    // The same elements as test_supplicant.py's independent authenticator expects.
    let (_, capable) = element(&format!("{TRANSITION} 8000"));
    assert_eq!(capable, hex("301a 0100 000fac04 0100 000fac04 0100 000fac02 8000 0000 000fac06"));
    let (_, required) = element(&format!("{PSK} c000"));
    assert_eq!(required, hex("301a 0100 000fac04 0100 000fac04 0100 000fac02 c000 0000 000fac06"));
    let (_, sha256) = element(&format!("{BOTH} 8000"));
    assert_eq!(sha256, hex("301a 0100 000fac04 0100 000fac04 0100 000fac06 8000 0000 000fac06"));
    let (_, unprotected) = element(&format!("{PSK_SHA256} 0000"));
    assert_eq!(unprotected, hex("3014 0100 000fac04 0100 000fac04 0100 000fac06 0000"));

    // The association request ends with that element; authentication is unchanged.
    let (ap, expected) = element(&format!("{BOTH} c000"));
    let mut frame = [0; 128];
    let n = station::request(&ap, [2, 0, 0, 0, 0, 1], true, &mut frame);
    assert!(frame[..n].ends_with(&expected));
    assert_eq!(frame[24] & 0x10, 0x10); // privacy
    assert_eq!(station::request(&ap, [2, 0, 0, 0, 0, 1], false, &mut frame), 30);
}

#[test]
fn key_frames_count_as_encrypted_by_the_marks_data_frames_need() {
    let (mac, ap) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2]);
    let clear = key_frame(&mac, &ap, false);
    let encrypted = key_frame(&mac, &ap, true);
    // Both are key frames from the access point; only one was decrypted.
    assert_eq!(station::eapol(&clear, &mac, &ap), Some(&clear[30..36]));
    assert_eq!(station::eapol(&encrypted, &mac, &ap), Some(&encrypted[30..36]));
    assert!(!station::decrypted(&clear));
    assert!(station::decrypted(&encrypted));
    // Another cipher, a cipher mismatch, a length mismatch, an integrity
    // error: not decrypted. The same frames are refused as data.
    for (at, value) in [(9, 0x00), (9, 0x20), (10, 0x04), (10, 0x08), (10, 0x10), (11, 0x01)] {
        let mut bad = encrypted.clone();
        bad[at] = value;
        assert!(!station::decrypted(&bad), "{at} {value}");
        bad[28..30].copy_from_slice(&[8, 0]);
        assert!(station::ethernet(&bad, &mac, &ap, &mut [0; 64], false).is_none());
    }
    // In 802.11 format the frame itself must also say it was protected.
    let mut native = vec![0u8; 16];
    native[..4].copy_from_slice(&[54, 0, 0, 0x40]);
    native[8..10].copy_from_slice(&[1, 0x40]);
    native.extend_from_slice(&[0x08, 0x42, 0, 0]);
    native.extend_from_slice(&[mac, ap, ap].concat());
    native.extend_from_slice(&[0, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e, 2, 3, 0, 2, 1, 2]);
    assert_eq!(station::eapol(&native, &mac, &ap), Some(&native[48..54]));
    assert!(station::decrypted(&native));
    native[17] = 0x02;
    assert_eq!(station::eapol(&native, &mac, &ap), Some(&native[48..54]));
    assert!(!station::decrypted(&native));
    for n in 0..native.len() {
        let _ = station::decrypted(&native[..n]);
    }
}

#[test]
fn try_again_later_is_told_from_a_refusal() {
    let (mac, ap) = ([2, 0, 0, 0, 0, 1], passive_scan::AccessPoint { bssid: [2, 0, 0, 0, 0, 2], ..Default::default() });
    let answer = |status: u16, elements: &[u8]| {
        let mut frame = vec![0x10, 0, 0, 0];
        frame.extend_from_slice(&[mac, ap.bssid, ap.bssid].concat());
        frame.extend_from_slice(&[0, 0, 0x11, 0]);
        frame.extend_from_slice(&status.to_le_bytes());
        frame.extend_from_slice(&[3, 0xc0]);
        frame.extend_from_slice(elements);
        station::response(&frame, &ap, mac, true).unwrap()
    };
    assert_eq!(answer(0, &[]), Ok(3));
    assert_eq!(answer(17, &[]), Err(49));
    assert_eq!(station::later(49), None);
    let wait = |elements: &[u8]| station::later(answer(30, elements).unwrap_err());
    // The comeback time is in time units of 1.024 ms; 25 ms are added.
    assert_eq!(wait(&[1, 1, 0x8c, 56, 5, 3, 0xe8, 3, 0, 0, 45, 0]), Some(1049));
    assert_eq!(wait(&[56, 5, 3, 100, 0, 0, 0]), Some(127));
    // None named, another kind of interval, or a broken element: one second.
    assert_eq!(wait(&[]), Some(1049));
    assert_eq!(wait(&[56, 5, 1, 100, 0, 0, 0]), Some(1049));
    assert_eq!(wait(&[56, 4, 3, 100, 0, 0]), Some(1049));
    assert_eq!(wait(&[56, 5, 3, 100, 0]), Some(1049));
    assert_eq!(wait(&[56]), Some(1049));
    // An access point cannot make Radio wait for long.
    assert_eq!(wait(&[56, 5, 3, 0xff, 0xff, 0xff, 0xff]), Some(1202));
    // Only an association response says it; an authentication with status 30 is a refusal.
    let mut auth = vec![0xb0, 0, 0, 0];
    auth.extend_from_slice(&[mac, ap.bssid, ap.bssid].concat());
    auth.extend_from_slice(&[0, 0, 0, 0, 2, 0, 30, 0]);
    assert_eq!(station::response(&auth, &ap, mac, false), Some(Err(49)));
}

// From here: Radio's driver against the register model.

const AP: [u8; 6] = [2, 5, 6, 7, 8, 9];
const STATION: [u8; 6] = [2, 17, 34, 51, 68, 85];
const TK: [u8; 16] = [42; 16];
const IGTK: [u8; 16] = [77; 16];

fn access_point(mfp: u8) -> passive_scan::AccessPoint {
    let mut ap = passive_scan::AccessPoint::default();
    ap.bssid = AP;
    ap.channel = 161;
    ap.wpa2_psk_ccmp = true;
    ap.akm = 2;
    ap.mfp = mfp;
    ap.ssid_len = 4;
    ap.ssid[..4].copy_from_slice(b"test");
    ap.rates[..3].copy_from_slice(&[0x8c, 0x98, 0xb0]);
    ap.rate_count = 3;
    ap
}

/// Associate with the model's access point; `keyed`: install the link's keys.
fn linked(mfp: u8, keyed: bool) -> wlan_axi::Axi {
    radio_io::reset();
    let mut axi = wlan_axi::Axi::started(STATION);
    assert_eq!(axi.associate(&access_point(mfp)), Ok(3));
    // No integrity group key before the pairwise key is in place, and never without protection.
    assert!(axi.install_igtk(4, &IGTK, &[3, 0, 0, 0, 0, 0]).is_ok() == (mfp != 0));
    if keyed {
        axi.install_ccmp(true, 0, &TK, &[]).unwrap();
        axi.install_ccmp(false, 1, &[24; 16], &[1, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(axi.install_igtk(4, &IGTK, &[3, 0, 0, 0, 0, 0]).is_ok(), mfp != 0);
        assert!(axi.install_igtk(3, &IGTK, &[3, 0, 0, 0, 0, 0]).is_err());
    }
    axi
}
fn joined(mfp: u8) -> wlan_axi::Axi {
    linked(mfp, true)
}
/// A joined link on which an unanswered SA Query ends the link (VIOLET_WIFI_SA_QUERY_TIMEOUT).
fn strict(mfp: u8) -> wlan_axi::Axi {
    let mut axi = joined(mfp);
    axi.pmf.timeout_ends(true);
    axi
}

/// An EAPOL-Key frame from the access point as the firmware hands it over
/// (Ethernet format), decrypted under the pairwise key or received in the clear.
fn key_frame(mac: &[u8; 6], ap: &[u8; 6], encrypted: bool) -> Vec<u8> {
    let mut p = vec![0u8; 64];
    p[..4].copy_from_slice(&[64, 0, 0, 0x40]);
    p[6] = 0x80;
    p[8] = 1;
    if encrypted {
        p[9] = 0x40;
    } else {
        p[10] = 0x04; // the peer has a key and the frame was not encrypted
    }
    p[16..22].copy_from_slice(mac);
    p[22..28].copy_from_slice(ap);
    p[28..30].copy_from_slice(&[0x88, 0x8e]);
    p[30..36].copy_from_slice(&[2, 3, 0, 2, 1, 2]);
    p
}
/// A unicast IP frame from the access point, decrypted by the firmware.
fn data_frame() -> Vec<u8> {
    let mut p = key_frame(&STATION, &AP, true);
    p[28..30].copy_from_slice(&[8, 0]);
    p
}
fn poll(axi: &mut wlan_axi::Axi) -> Result<(usize, usize), u32> {
    axi.poll_data(&mut [0u8; 1536], &mut [0u8; 1536])
}
/// Poll for `ms` of model time: the first result that is not "nothing".
fn poll_for(axi: &mut wlan_axi::Axi, ms: u32) -> Result<(usize, usize), u32> {
    let start = radio_io::now();
    while radio_io::now().wrapping_sub(start) < ms {
        match poll(axi) {
            Ok((0, 0)) => {}
            other => return other,
        }
    }
    Ok((0, 0))
}
fn sent_management() -> Vec<Vec<u8>> {
    radio_io::with(|m| m.management.clone())
}

#[test]
fn forged_deauthentication_ends_only_an_unprotected_link() {
    use pmf::access_point::{ccmp_protect, frame};
    // Without 802.11w one frame with the access point's address is enough.
    for to in [STATION, [255; 6]] {
        let mut axi = joined(0);
        radio_io::with(|m| m.receive_management(&frame(0xc0, &AP, &to, &[7, 0])));
        assert_eq!(poll(&mut axi), Err(54));
    }
    // So it is on a protected link before its keys are installed.
    let mut axi = linked(2, false);
    radio_io::with(|m| m.receive_management(&frame(0xc0, &AP, &STATION, &[3, 0])));
    assert_eq!(poll(&mut axi), Err(54));
    for to in [STATION, [255; 6]] {
        for kind in [0xc0, 0xa0] {
            // With 802.11w, a forged leave is dropped: nothing sent, nothing ends.
            let mut axi = strict(1);
            for reason in [1, 2, 3, 4, 5, 8] {
                radio_io::with(|m| m.receive_management(&frame(kind, &AP, &to, &[reason, 0])));
                assert_eq!(poll_for(&mut axi, 3000), Ok((0, 0)));
            }
            assert!(sent_management().is_empty());
            // "You are not associated" starts an SA Query. This access point
            // never answers. As shipped, the link stays all the same...
            let mut axi = joined(1);
            radio_io::with(|m| m.receive_management(&frame(kind, &AP, &to, &[7, 0])));
            assert_eq!(poll(&mut axi), Ok((0, 0)));
            let query = sent_management();
            assert_eq!(query.len(), 1);
            assert_eq!((query[0][0], query[0][1], query[0].len()), (0xd0, 0x40, 44));
            assert_eq!((&query[0][4..10], &query[0][16..22]), (&AP[..], &AP[..]));
            assert_eq!(poll_for(&mut axi, 30_000), Ok((0, 0)));
            assert!((4..=7).contains(&sent_management().len()), "{}", sent_management().len());
            // ...and where the build says so it ends after a second of asking.
            let mut axi = strict(1);
            radio_io::with(|m| m.receive_management(&frame(kind, &AP, &to, &[7, 0])));
            assert_eq!(poll(&mut axi), Ok((0, 0)));
            let started = radio_io::now();
            assert_eq!(poll_for(&mut axi, 30_000), Err(54));
            let waited = radio_io::now() - started;
            assert!((1000..1100).contains(&waited), "{waited}");
            assert!((4..=7).contains(&sent_management().len()), "{}", sent_management().len());
        }
    }
    // An answered query keeps the link, whatever is forged afterwards.
    let mut axi = strict(1);
    radio_io::with(|m| m.receive_management(&frame(0xa0, &AP, &STATION, &[7, 0])));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    // The access point answers the transaction (a link's first query has ID 1),
    // protected with the pairwise key.
    let answer = ccmp_protect(&frame(0xd0, &AP, &STATION, &[8, 1, 1, 0]), &TK, 1);
    radio_io::with(|m| m.receive_management(&answer));
    assert_eq!(poll_for(&mut axi, 10_000), Ok((0, 0)));
    assert_eq!(sent_management().len(), 1);
}

#[test]
fn an_answer_that_waits_in_the_ring_counts_however_late_it_is_read() {
    use pmf::access_point::{ccmp_protect, frame};
    let answer = ccmp_protect(&frame(0xd0, &AP, &STATION, &[8, 1, 1, 0]), &TK, 1);
    let beacon = frame(0x80, &AP, &[255; 6], &[0; 12]);
    // The access point answers at once, behind two beacons, and Radio does
    // not look at its rings for two seconds (a request, a blocking send).
    let mut axi = strict(2);
    radio_io::with(|m| m.receive_management(&frame(0xc0, &AP, &STATION, &[7, 0])));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    radio_io::with(|m| {
        m.receive_management(&beacon);
        m.receive_management(&beacon);
        m.receive_management(&answer);
        m.advance(2000);
    });
    assert_eq!(poll_for(&mut axi, 10_000), Ok((0, 0)));
    // Radio keeps polling, and the answer arrives as the second runs out,
    // behind beacons that are read first.
    let mut axi = strict(2);
    radio_io::with(|m| m.receive_management(&frame(0xc0, &AP, &STATION, &[7, 0])));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    assert_eq!(poll_for(&mut axi, 985), Ok((0, 0)));
    radio_io::with(|m| {
        for _ in 0..8 {
            m.receive_management(&beacon);
        }
        m.receive_management(&answer);
    });
    assert_eq!(poll_for(&mut axi, 10_000), Ok((0, 0)));
    // The answer was read by somebody else during the pause (a rekey's wait
    // for its event discards what it finds): the query is asked again and the
    // access point gets a second from then, not from the forged frame.
    let mut axi = strict(2);
    radio_io::with(|m| m.receive_management(&frame(0xc0, &AP, &STATION, &[7, 0])));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    radio_io::with(|m| m.advance(2000));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    assert_eq!(sent_management().len(), 2);
    assert_eq!(poll_for(&mut axi, 500), Ok((0, 0)));
    let again = ccmp_protect(&frame(0xd0, &AP, &STATION, &[8, 1, 1, 0]), &TK, 2);
    radio_io::with(|m| m.receive_management(&again));
    assert_eq!(poll_for(&mut axi, 10_000), Ok((0, 0)));
}

#[test]
fn a_link_in_doubt_ends_only_when_what_is_sent_brings_nothing_back() {
    use pmf::access_point::frame;
    let mut ethernet = [0u8; 64];
    ethernet[..6].copy_from_slice(&AP);
    ethernet[6..12].copy_from_slice(&STATION);
    ethernet[12..14].copy_from_slice(&[8, 0]);
    let doubted = || {
        let mut axi = joined(2);
        radio_io::with(|m| m.receive_management(&frame(0xc0, &AP, &STATION, &[7, 0])));
        assert_eq!(poll_for(&mut axi, 2000), Ok((0, 0)));
        axi
    };
    // The access point is gone: eight frames into the silence, five seconds.
    let mut axi = doubted();
    let started = radio_io::now();
    for _ in 0..7 {
        axi.send_ip(&ethernet).unwrap();
    }
    assert_eq!(poll_for(&mut axi, 20_000), Ok((0, 0)));
    axi.send_ip(&ethernet).unwrap();
    assert_eq!(poll(&mut axi), Err(54));
    assert!(radio_io::now() - started >= 5000);
    let mut axi = doubted();
    for _ in 0..50 {
        axi.send_ip(&ethernet).unwrap();
    }
    assert_eq!(poll_for(&mut axi, 3500), Ok((0, 0)));
    assert_eq!(poll_for(&mut axi, 20_000), Err(54));
    // The access point is there: one unicast frame under the pairwise key
    // and the doubt is over. A broadcast frame, which every member of the
    // network can forge, does not count.
    let mut axi = doubted();
    for _ in 0..50 {
        axi.send_ip(&ethernet).unwrap();
    }
    let mut broadcast = data_frame();
    broadcast[16..22].fill(0xff);
    radio_io::with(|m| m.receive_packet(&data_frame()));
    assert_eq!(poll(&mut axi), Ok((48, 0)));
    for _ in 0..50 {
        axi.send_ip(&ethernet).unwrap();
    }
    assert_eq!(poll_for(&mut axi, 60_000), Ok((0, 0)));
    let mut axi = doubted();
    for _ in 0..50 {
        axi.send_ip(&ethernet).unwrap();
    }
    radio_io::with(|m| m.receive_packet(&broadcast));
    assert_eq!(poll(&mut axi), Ok((48, 0)));
    assert_eq!(poll_for(&mut axi, 20_000), Err(54));
}

#[test]
fn a_key_frame_in_the_clear_does_not_reach_hostap_on_a_protected_link() {
    let clear = key_frame(&STATION, &AP, false);
    let encrypted = key_frame(&STATION, &AP, true);
    // The first handshake's frames come in the clear: delivered.
    let mut axi = linked(2, false);
    radio_io::with(|m| m.receive_packet(&clear));
    assert_eq!(poll(&mut axi), Ok((0, 6)));
    // With the keys in place only an encrypted key frame (a rekey) is the
    // access point's. A forged one is dropped here, whatever follows it.
    for mfp in [1, 2] {
        let mut axi = joined(mfp);
        for _ in 0..40 {
            radio_io::with(|m| m.receive_packet(&clear));
            assert_eq!(poll(&mut axi), Ok((0, 0)));
        }
        radio_io::with(|m| {
            m.receive_packet(&clear);
            m.receive_packet(&encrypted);
        });
        let mut key = [0u8; 1536];
        assert_eq!(axi.poll_data(&mut [0u8; 1536], &mut key), Ok((0, 0)));
        assert_eq!(axi.poll_data(&mut [0u8; 1536], &mut key), Ok((0, 6)));
        assert_eq!(key[..6], encrypted[30..36]);
    }
    // Without 802.11w nothing changes: hostap sees both (and since this
    // change ends the link only when it decides to: test_supplicant.py).
    let mut axi = joined(0);
    radio_io::with(|m| m.receive_packet(&clear));
    assert_eq!(poll(&mut axi), Ok((0, 6)));
}

#[test]
fn authentic_deauthentication_ends_a_protected_link_and_sa_queries_are_answered() {
    use pmf::access_point::{bip_protect, ccmp_protect, frame};
    let mut axi = joined(2);
    // The access point checks that we are still here: answer, stay joined.
    let request = ccmp_protect(&frame(0xd0, &AP, &STATION, &[8, 0, 0x21, 0x43]), &TK, 1);
    radio_io::with(|m| m.receive_management(&request));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    let mut response = frame(0xd0, &STATION, &AP, &[8, 1, 0x21, 0x43]);
    response[16..22].copy_from_slice(&AP); // address 3 is the BSSID in both directions
    let expected = ccmp_protect(&response, &TK, 1);
    assert_eq!(sent_management().len(), 1);
    assert_eq!(sent_management()[0][24..], expected[24..]);
    // A replay of that request is not answered again.
    radio_io::with(|m| m.receive_management(&request));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    assert_eq!(sent_management().len(), 1);
    // A protected unicast deauthentication ends the link.
    let leave = ccmp_protect(&frame(0xc0, &AP, &STATION, &[3, 0]), &TK, 2);
    radio_io::with(|m| m.receive_management(&leave));
    assert_eq!(poll(&mut axi), Err(54));
    // The link's keys went with it: nothing is answered or trusted any more.
    assert!(!axi.pmf.active());
    radio_io::with(|m| m.receive_management(&ccmp_protect(&frame(0xd0, &AP, &STATION, &[8, 0, 1, 2]), &TK, 3)));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    assert_eq!(sent_management().len(), 1);
    // So does a broadcast one with a valid BIP MIC, but not one replayed or under another key.
    let mut axi = joined(2);
    radio_io::with(|m| m.receive_management(&bip_protect(frame(0xc0, &AP, &[255; 6], &[3, 0]), &IGTK, 4, 3)));
    radio_io::with(|m| m.receive_management(&bip_protect(frame(0xc0, &AP, &[255; 6], &[3, 0]), &TK, 4, 9)));
    assert_eq!(poll_for(&mut axi, 3000), Ok((0, 0)));
    assert!(sent_management().is_empty());
    radio_io::with(|m| m.receive_management(&bip_protect(frame(0xc0, &AP, &[255; 6], &[3, 0]), &IGTK, 4, 4)));
    assert_eq!(poll(&mut axi), Err(54));
    // A new association forgets the old keys: its first frames follow the old rule.
    let mut axi = joined(2);
    assert_eq!(axi.associate(&access_point(2)), Ok(3));
    assert!(axi.pmf.active());
    radio_io::with(|m| m.receive_management(&ccmp_protect(&frame(0xd0, &AP, &STATION, &[8, 0, 1, 2]), &TK, 5)));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    assert!(sent_management().is_empty());
}

#[test]
fn leaving_a_link_wipes_its_keys() {
    use pmf::access_point::{ccmp_protect, frame};
    let mut axi = joined(2);
    axi.leave();
    assert!(!axi.pmf.active());
    assert!(axi.install_igtk(4, &IGTK, &[9, 0, 0, 0, 0, 0]).is_err());
    // What the old pairwise key protects is no longer understood.
    radio_io::with(|m| m.receive_management(&ccmp_protect(&frame(0xd0, &AP, &STATION, &[8, 0, 1, 2]), &TK, 1)));
    assert_eq!(poll(&mut axi), Ok((0, 0)));
    assert!(sent_management().is_empty());
}

#[test]
fn association_is_tried_again_when_the_access_point_says_come_back() {
    let requests = || radio_io::with(|m| m.association_requests.clone());
    let channel_commands = || radio_io::with(|m| m.commands.iter().filter(|c| c.len() > 66 && c[36] == 0x1c).map(|c| c[66]).collect::<Vec<_>>());
    // (answers "later", time units named, expected result, wait between the requests)
    for (later, units, expected, wait) in [
        (0, 0, Ok(3), 0),
        (1, 1000, Ok(3), 1049),
        (1, 200, Ok(3), 229),
        (1, 0, Ok(3), 1049),
        (1, 1_000_000, Ok(3), 1202),
        // Still "later" the second time: the join fails as a refusal does.
        (2, 1000, Err(49), 1049),
    ] {
        radio_io::reset();
        radio_io::with(|m| (m.comeback, m.comeback_units) = (later, units));
        let mut axi = wlan_axi::Axi::started(STATION);
        assert_eq!(axi.associate(&access_point(2)), expected, "{later} {units}");
        // One channel grant covers it, given back after either outcome.
        assert_eq!(channel_commands(), [0, 1]);
        let asked = requests();
        if later == 0 {
            assert_eq!(asked.len(), 1);
            continue;
        }
        assert_eq!(asked.len(), 2);
        let waited = asked[1] - asked[0];
        assert!((wait..wait + 60).contains(&waited), "{units}: {waited}");
        // All of it inside the shortest grant the driver accepts (1.6 s).
        assert!(waited < 1300);
    }
    // A refusal is not retried.
    radio_io::reset();
    radio_io::with(|m| m.deny_association = true);
    let mut axi = wlan_axi::Axi::started(STATION);
    assert_eq!(axi.associate(&access_point(2)), Err(49));
    assert_eq!(requests().len(), 1);
}

#[test]
fn sae_frames_go_to_and_come_from_the_access_point() {
    let mut ap = access_point(2);
    ap.akm = 8;
    let mut rsn = [0; station::RSN_MAX];
    let n = station::rsne(&ap, &mut rsn);
    assert_eq!(rsn[16..20], [0, 15, 172, 8], "SAE in the RSN element");
    assert_eq!(rsn[20], 0xc0, "with protection capable and required");
    assert_eq!(n, station::RSN_MAX);
    // SAE never goes out without MFPC, even for an access point the scan
    // left at mfp 0.
    let mut loose = ap;
    loose.mfp = 0;
    let n = station::rsne(&loose, &mut rsn);
    assert_eq!((n, rsn[16..20].to_vec(), rsn[20]), (station::RSN_MAX, vec![0, 15, 172, 8], 0x80));
    assert_eq!(rsn[22..], [0, 0, 0, 15, 172, 6], "with BIP-CMAC-128");
    let pwe = sae::hunting_and_pecking(b"password", &STATION, &AP).unwrap();
    let mut n = 0u8;
    let mut s = sae::Sae::new(&pwe, false, &mut |b: &mut [u8; 32]| {
        n += 1;
        b.fill(n);
        true
    })
    .unwrap();
    let mut body = [0u8; sae::BODY_MAX];
    let len = s.message(&mut body);
    let mut frame = [0u8; 24 + sae::BODY_MAX];
    let f = station::sae_request(&ap, STATION, &body[..len], &mut frame);
    assert_eq!(f, 24 + 104);
    assert_eq!(frame[0], 0xb0);
    assert_eq!(frame[4..10], AP);
    assert_eq!(frame[10..16], STATION);
    assert_eq!(frame[24..30], [3, 0, 1, 0, 0, 0]);
    // The access point's answer, as it would arrive.
    let mut reply = frame[..f].to_vec();
    reply[4..10].copy_from_slice(&STATION);
    reply[10..16].copy_from_slice(&AP);
    assert_eq!(station::sae_response(&reply, &ap, STATION), Some(&reply[24..]));
    // Not from this access point, not SAE, or protected: not for the exchange.
    for (at, value) in [(10, 9u8), (24, 0), (1, 0x40), (0, 0x10)] {
        let mut bad = reply.clone();
        bad[at] = value;
        assert_eq!(station::sae_response(&bad, &ap, STATION), None, "{at}");
    }
    assert_eq!(station::sae_response(&reply[..29], &ap, STATION), None);
}
