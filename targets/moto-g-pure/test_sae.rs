//! Host tests for WPA3-Personal SAE (sae.rs): IEEE 802.11-2020 Annex J.10
//! (hunting-and-pecking commit, KCK, PMK, PMKID; hash-to-element PWE), whole
//! exchanges between two instances, the anti-clogging token, the checks a
//! peer's commit must pass, and the AKM choice in transition mode.
//! `make test-unit` runs these; `make test-sae` builds this file again with
//! `--cfg hostap` and runs the exchange against hostap's own SAE.
#![allow(dead_code)]
mod sae;

use sae::{Akm, Sae, State, Step, BODY_MAX};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
fn arr<const L: usize>(s: &str) -> [u8; L] {
    hex(s).try_into().unwrap()
}

// IEEE 802.11-2020 Annex J.10, as hostap's common_module_tests.c carries it.
const ADDR1: [u8; 6] = [0x4d, 0x3f, 0x2f, 0xff, 0xe3, 0x87];
const ADDR2: [u8; 6] = [0xa5, 0xd8, 0xaa, 0x95, 0x8e, 0x3c];
const PASSWORD: &[u8] = b"mekmitasdigoat";
const RAND: &str = "992465fd3daa3c60aa6565b7f62a2a7f2e12dd12f198faf4fbed89d7ff1ace94";
const MASK: &str = "9507a90f777a044d6a0830b91ea3d5dd70bece44e1acffb86983b5e1bf9fb322";
const LOCAL_COMMIT: &str = "13002e2c0f0db52440ad146d967114ce005ce1eab0aa2c2e5c2871b774f6c2575c65d5ad9e00829707aa36ba8b859738fc961d08243505f47c035376d7ac4bc8d7b95083bf43827d0fc31ed778dd3671fd21a46d1091d64b6f9a1e1272621325dbe1";
const PEER_COMMIT: &str = "1300591b96f3397fb945100848e7b550543b6720d88337ee93fc49fd6df7e08b5223e71b9bb048d3873f20556953a96c91536fd8ee6ca9b4a68a148b056a909be03e83ae208f60f8ef5537858074db06687032399862999b511e0a1552a5fea317c2";
const KCK: &str = "1e733f6d9bd53256287304338831b09a39406d121017073a5c30db36f36cb81a";
const PMK: &str = "4e4dfab1a2dd8ac1a91790f953faaa452ae5c6873ab75b63605ba663f8a7fe59";
const PMKID: &str = "8747a600eea3f9f22475df58ca1e5498";
const H2E_ADDR1: [u8; 6] = [0x00, 0x09, 0x5b, 0x66, 0xec, 0x1e];
const H2E_ADDR2: [u8; 6] = [0x00, 0x0b, 0x6b, 0xd9, 0x02, 0x46];
const H2E_PWE: &str = "c93049b9e64000f848201649e999f2b5c22dea69b5632c9df4d633b8aa1f6c1e73634e94b53d82e7383a8d258199d9dc1a5ee8269d060382ccbf33e614ff59a0";

fn body(seq: u16, status: u16, rest: &[u8]) -> Vec<u8> {
    let mut b = vec![3, 0];
    b.extend_from_slice(&seq.to_le_bytes());
    b.extend_from_slice(&status.to_le_bytes());
    b.extend_from_slice(rest);
    b
}
fn message(s: &mut Sae) -> Vec<u8> {
    let mut out = [0u8; BODY_MAX];
    let n = s.message(&mut out);
    out[..n].to_vec()
}
/// A deterministic generator for tests (not for use: it only has to differ).
fn counter(seed: u8) -> impl FnMut(&mut [u8; 32]) -> bool {
    let mut n = seed;
    move |b: &mut [u8; 32]| {
        n = n.wrapping_add(1);
        for (i, x) in b.iter_mut().enumerate() {
            *x = n.wrapping_mul(31).wrapping_add(i as u8 * 7) ^ 0x5a;
        }
        b[0] &= 0x7f;
        true
    }
}
fn annex() -> Sae {
    let pwe = sae::hunting_and_pecking(PASSWORD, &ADDR1, &ADDR2).unwrap();
    Sae::with(&pwe, false, &sae::from_be(&hex(RAND)), &sae::from_be(&hex(MASK))).unwrap()
}

#[test]
fn annex_j10_hunting_and_pecking() {
    let mut s = annex();
    let commit = message(&mut s);
    assert_eq!(commit[..6], [3, 0, 1, 0, 0, 0]);
    assert_eq!(commit[6..], hex(LOCAL_COMMIT)[..], "commit scalar and element");
    assert_eq!(s.receive(&body(1, 0, &hex(PEER_COMMIT))), Step::Send);
    assert_eq!(s.state, State::Confirmed);
    // The keys (annex_j10_keys) are released only after the peer's confirm.
    let confirm = message(&mut s);
    assert_eq!(confirm[..8], [3, 0, 2, 0, 0, 0, 1, 0]);
    assert!(s.pmk().is_none(), "no keys before the peer's confirm");
}

#[test]
fn annex_j10_hash_to_element() {
    let pt = sae::pt(b"byteme", PASSWORD, Some(b"psk4internet")).unwrap();
    let pwe = sae::pwe_from_pt(&pt, &H2E_ADDR1, &H2E_ADDR2).unwrap();
    assert_eq!(pwe.to_vec(), hex(H2E_PWE));
    // The order of the addresses does not matter.
    assert_eq!(sae::pwe_from_pt(&pt, &H2E_ADDR2, &H2E_ADDR1).unwrap(), pwe);
}

/// Two instances run the whole exchange; returns both, accepted.
fn exchange(h2e: bool, a_pw: &[u8], b_pw: &[u8]) -> (Sae, Sae, Step) {
    let (sta, ap) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2]);
    let pwe = |pw: &[u8]| {
        if h2e {
            sae::pwe_from_pt(&sae::pt(b"violet", pw, None).unwrap(), &sta, &ap).unwrap()
        } else {
            sae::hunting_and_pecking(pw, &sta, &ap).unwrap()
        }
    };
    let mut a = Sae::new(&pwe(a_pw), h2e, &mut counter(1)).unwrap();
    let mut b = Sae::new(&pwe(b_pw), h2e, &mut counter(77)).unwrap();
    let (ca, cb) = (message(&mut a), message(&mut b));
    assert_eq!(a.receive(&cb), Step::Send);
    assert_eq!(b.receive(&ca), Step::Send);
    let (fa, fb) = (message(&mut a), message(&mut b));
    let last = b.receive(&fa);
    let first = a.receive(&fb);
    assert_eq!(first, last);
    (a, b, last)
}

#[test]
fn a_whole_exchange_agrees_on_the_keys() {
    for h2e in [false, true] {
        let (a, b, step) = exchange(h2e, b"correct horse", b"correct horse");
        assert_eq!(step, Step::Done, "h2e {h2e}");
        let (pa, ia) = a.pmk().unwrap();
        let (pb, ib) = b.pmk().unwrap();
        assert_eq!(pa, pb);
        assert_eq!(ia, ib);
    }
}

#[test]
fn a_wrong_password_never_confirms() {
    for h2e in [false, true] {
        let (a, b, step) = exchange(h2e, b"correct horse", b"wrong horse");
        assert_eq!(step, Step::Ignore, "a bad confirm is dropped");
        assert!(a.pmk().is_none() && b.pmk().is_none());
    }
}

#[test]
fn annex_j10_keys() {
    let mut s = annex();
    assert_eq!(s.receive(&body(1, 0, &hex(PEER_COMMIT))), Step::Send);
    let (kck, pmk, pmkid) = s.derived();
    assert_eq!(kck.to_vec(), hex(KCK));
    assert_eq!(pmk.to_vec(), hex(PMK));
    assert_eq!(pmkid.to_vec(), hex(PMKID));
}

#[test]
fn a_reflected_commit_is_dropped_and_bad_ones_fail() {
    let mut s = annex();
    let own = message(&mut s);
    assert_eq!(s.receive(&own), Step::Ignore, "reflection");
    assert_eq!(s.state, State::Committed);
    let peer = hex(PEER_COMMIT);
    let n = hex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");
    for scalar in [vec![0u8; 32], { let mut v = vec![0u8; 32]; v[31] = 1; v }, n.clone(), vec![0xff; 32]] {
        let mut s = annex();
        let mut c = peer.clone();
        c[2..34].copy_from_slice(&scalar);
        assert_eq!(s.receive(&body(1, 0, &c)), Step::Fail(1));
    }
    // An element off the curve, or with a coordinate of p or more.
    let mut c = peer.clone();
    c[97] ^= 1;
    assert_eq!(annex().receive(&body(1, 0, &c)), Step::Fail(1));
    let mut c = peer.clone();
    c[34..66].copy_from_slice(&[0xff; 32]);
    assert_eq!(annex().receive(&body(1, 0, &c)), Step::Fail(1));
    // Another group, a short frame, an H2E commit to an H&P exchange.
    let mut c = peer.clone();
    c[0] = 20;
    assert_eq!(annex().receive(&body(1, 0, &c)), Step::Fail(77));
    assert_eq!(annex().receive(&body(1, 0, &peer[..60])), Step::Fail(77));
    assert_eq!(annex().receive(&body(1, 126, &peer)), Step::Fail(126));
    assert_eq!(annex().receive(&body(1, 77, &[19, 0])), Step::Fail(77));
    // Not SAE, or a confirm before any commit: not for us.
    let mut other = body(1, 0, &peer);
    other[0] = 0;
    assert_eq!(annex().receive(&other), Step::Ignore);
    assert_eq!(annex().receive(&body(2, 0, &[1, 0])), Step::Ignore);
}

#[test]
fn the_anti_clogging_token_is_sent_back() {
    // Hunting-and-pecking: the token follows the group.
    let mut s = annex();
    let token = [0xa5u8; 32];
    let mut req = vec![19, 0];
    req.extend_from_slice(&token);
    assert_eq!(s.receive(&body(1, 76, &req)), Step::Send);
    let c = message(&mut s);
    assert_eq!(c[8..40], token);
    assert_eq!(c[40..], hex(LOCAL_COMMIT)[2..]);
    // Hash-to-element: in a container element after the element.
    let pt = sae::pt(b"byteme", PASSWORD, None).unwrap();
    let pwe = sae::pwe_from_pt(&pt, &H2E_ADDR1, &H2E_ADDR2).unwrap();
    let mut h = Sae::new(&pwe, true, &mut counter(3)).unwrap();
    let plain = message(&mut h);
    assert_eq!(plain[4..6], [126, 0]);
    let mut req = vec![19, 0, 255, 33, 93];
    req.extend_from_slice(&token);
    assert_eq!(h.receive(&body(1, 76, &req)), Step::Send);
    let c = message(&mut h);
    assert_eq!(c[..plain.len()], plain[..]);
    assert_eq!(c[plain.len()..plain.len() + 3], [255, 33, 93]);
    assert_eq!(c[plain.len() + 3..], token);
    // A malformed container, or a token request that never ends, fails.
    assert_eq!(Sae::new(&pwe, true, &mut counter(4)).unwrap().receive(&body(1, 76, &[19, 0, 221, 3, 1, 2, 3])), Step::Fail(76));
    let mut s = annex();
    let mut req = vec![19, 0];
    req.extend_from_slice(&token);
    for _ in 0..3 {
        assert_eq!(s.receive(&body(1, 76, &req)), Step::Send);
    }
    assert_eq!(s.receive(&body(1, 76, &req)), Step::Fail(76));
}

#[test]
fn retransmissions_are_bounded() {
    let mut s = annex();
    for _ in 0..5 {
        assert_eq!(s.timeout(), Step::Send);
    }
    assert_eq!(s.timeout(), Step::Fail(0));
    // In Confirmed, a repeated peer commit resends the confirm with the next counter.
    let mut s = annex();
    s.receive(&body(1, 0, &hex(PEER_COMMIT)));
    assert_eq!(message(&mut s)[6..8], [1, 0]);
    assert_eq!(s.receive(&body(1, 0, &hex(PEER_COMMIT))), Step::Send);
    assert_eq!(message(&mut s)[6..8], [2, 0]);
    // A different commit then is ignored; a refusal of our confirm fails.
    let mut c = hex(PEER_COMMIT);
    c[10] ^= 1;
    assert_eq!(s.receive(&body(1, 0, &c)), Step::Ignore);
    assert_eq!(s.receive(&body(2, 1, &[])), Step::Fail(1));
}

#[test]
fn transition_mode_prefers_sae() {
    use sae::choose;
    // WPA2/WPA3 transition: PSK and SAE, MFP capable.
    assert_eq!(choose(&[2, 8], true, false, false, true, true, false), Some(Akm::Sae { h2e: false }));
    assert_eq!(choose(&[2, 8], true, true, false, true, true, false), Some(Akm::Sae { h2e: true }));
    // Without the password (only a PMK kept), PSK.
    assert_eq!(choose(&[2, 8], true, false, false, false, true, false), Some(Akm::Psk { sha256: false }));
    // SAE needs management frame protection.
    assert_eq!(choose(&[2, 8], false, false, false, true, true, false), Some(Akm::Psk { sha256: false }));
    assert_eq!(choose(&[6, 8], true, false, false, false, true, false), Some(Akm::Psk { sha256: true }));
    // WPA3-only, and H2E-only networks, cannot fall back.
    assert_eq!(choose(&[8], true, false, false, false, true, false), None);
    assert_eq!(choose(&[2, 8], true, true, true, false, true, false), None);
    assert_eq!(choose(&[2, 8], true, true, true, true, false, false), Some(Akm::Sae { h2e: true }));
    assert_eq!(choose(&[2], true, false, false, true, false, false), None);

}

#[test]
fn a_network_known_to_take_sae_never_falls_back_to_psk() {
    use sae::choose;
    // A rogue access point with the network's SSID offering only PSK, or SAE
    // without management frame protection: no handshake at all.
    assert_eq!(choose(&[2], false, false, false, true, true, true), None);
    assert_eq!(choose(&[6], true, false, false, true, true, true), None);
    assert_eq!(choose(&[2, 8], false, false, false, true, true, true), None);
    // The real one still joins with SAE.
    assert_eq!(choose(&[2, 8], true, false, false, true, true, true), Some(Akm::Sae { h2e: false }));
    // Without the password it cannot: still no PSK.
    assert_eq!(choose(&[2, 8], true, false, false, false, true, true), None);
}

#[test]
fn transition_disable_is_found_in_the_key_data() {
    use sae::{transition_disable, TRANSITION_DISABLE_SAE};
    // An RSN element, the GTK KDE, Transition Disable, then padding.
    let mut kd = vec![48, 2, 1, 0];
    kd.extend_from_slice(&[0xdd, 6, 0, 15, 172, 1, 1, 0]);
    kd.extend_from_slice(&[0xdd, 5, 0, 15, 172, 32, TRANSITION_DISABLE_SAE]);
    kd.extend_from_slice(&[0xdd, 0, 0]);
    assert_eq!(transition_disable(&kd), Some(TRANSITION_DISABLE_SAE));
    // None without it, after padding, or in a truncated KDE.
    assert_eq!(transition_disable(&kd[..12]), None);
    let mut padded = kd[..12].to_vec();
    padded.extend_from_slice(&[0xdd, 0, 0xdd, 5, 0, 15, 172, 32, 1]);
    assert_eq!(transition_disable(&padded), None);
    assert_eq!(transition_disable(&kd[..18]), None);
    assert_eq!(transition_disable(&[0xdd, 5, 0, 15, 172, 33, 1]), None, "another KDE");
}

#[test]
fn a_rejected_groups_element_from_the_access_point_does_not_salt_the_keys() {
    let (sta, ap) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2]);
    let pwe = sae::pwe_from_pt(&sae::pt(b"violet", b"correct horse", None).unwrap(), &sta, &ap).unwrap();
    let mut a = Sae::new(&pwe, true, &mut counter(1)).unwrap();
    let mut b = Sae::new(&pwe, true, &mut counter(77)).unwrap();
    let (ca, mut cb) = (message(&mut a), message(&mut b));
    // Added on the air to the access point's commit: Rejected Groups, group 20.
    cb.extend_from_slice(&[255, 3, 92, 20, 0]);
    assert_eq!(a.receive(&cb), Step::Send);
    assert_eq!(b.receive(&ca), Step::Send);
    let (fa, fb) = (message(&mut a), message(&mut b));
    assert_eq!(b.receive(&fa), Step::Done);
    assert_eq!(a.receive(&fb), Step::Done);
    assert_eq!(a.pmk().unwrap(), b.pmk().unwrap());
}

#[cfg(hostap)]
mod hostap {
    //! The other side is hostap 2.11's common/sae.c on OpenSSL, with the
    //! access point's anti-clogging token from ap/comeback_token.c
    //! (test_sae_hostap.c).
    use super::*;
    extern "C" {
        fn ref_new(h2e: i32, own: *const u8, peer: *const u8, pw: *const u8, pw_len: usize, ssid: *const u8, ssid_len: usize) -> *mut u8;
        fn ref_commit(s: *mut u8, token: *const u8, token_len: usize, out: *mut u8) -> usize;
        fn ref_parse_commit(s: *mut u8, data: *const u8, len: usize, token: *mut u8, token_len: *mut usize) -> i32;
        fn ref_process(s: *mut u8) -> i32;
        fn ref_confirm(s: *mut u8, out: *mut u8) -> usize;
        fn ref_check_confirm(s: *mut u8, data: *const u8, len: usize) -> i32;
        fn ref_keys(s: *mut u8, pmk: *mut u8, pmkid: *mut u8);
        fn ref_token_request(h2e: i32, sta: *const u8, out: *mut u8) -> usize;
        fn ref_token_check(sta: *const u8, token: *const u8, len: usize) -> i32;
        fn ref_free(s: *mut u8);
    }
    const STA: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    const AP: [u8; 6] = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

    fn run(h2e: bool, ours: &[u8], theirs: &[u8], clog: bool) -> Option<([u8; 32], [u8; 16])> {
        let ssid = b"violet-wpa3";
        let pwe = if h2e {
            sae::pwe_from_pt(&sae::pt(ssid, ours, None).unwrap(), &STA, &AP).unwrap()
        } else {
            sae::hunting_and_pecking(ours, &STA, &AP).unwrap()
        };
        let mut rng = |b: &mut [u8; 32]| {
            let f = std::fs::File::open("/dev/urandom");
            use std::io::Read;
            f.and_then(|mut f| f.read_exact(b)).is_ok()
        };
        let mut sta = Sae::new(&pwe, h2e, &mut rng).unwrap();
        unsafe {
            let ap = ref_new(h2e as i32, AP.as_ptr(), STA.as_ptr(), theirs.as_ptr(), theirs.len(), ssid.as_ptr(), ssid.len());
            assert!(!ap.is_null());
            let mut commit = message(&mut sta);
            if clog {
                // The access point is under load: it asks for a token first.
                let mut req = [0u8; 64];
                let n = ref_token_request(h2e as i32, STA.as_ptr(), req.as_mut_ptr());
                assert_eq!(sta.receive(&body(1, 76, &req[..n])), Step::Send);
                commit = message(&mut sta);
            }
            let mut token = [0u8; 256];
            let mut token_len = 0usize;
            let status = ref_parse_commit(ap, commit[6..].as_ptr(), commit.len() - 6, token.as_mut_ptr(), &mut token_len);
            assert_eq!(status, 0, "hostap took our commit");
            if clog {
                assert_eq!(ref_token_check(STA.as_ptr(), token.as_ptr(), token_len), 0, "hostap's token came back intact");
            } else {
                assert_eq!(token_len, 0);
            }
            assert_eq!(ref_process(ap), 0);
            let mut out = [0u8; 512];
            let n = ref_commit(ap, core::ptr::null(), 0, out.as_mut_ptr());
            let status = if h2e { 126 } else { 0 };
            assert_eq!(sta.receive(&body(1, status, &out[..n])), Step::Send);
            let ours = message(&mut sta);
            let accepted = ref_check_confirm(ap, ours[6..].as_ptr(), ours.len() - 6) == 0;
            let n = ref_confirm(ap, out.as_mut_ptr());
            let step = sta.receive(&body(2, 0, &out[..n]));
            let mut pmk = [0u8; 32];
            let mut pmkid = [0u8; 16];
            ref_keys(ap, pmk.as_mut_ptr(), pmkid.as_mut_ptr());
            ref_free(ap);
            assert_eq!(accepted, step == Step::Done, "both sides agree on the outcome");
            if !accepted {
                return None;
            }
            let (p, i) = sta.pmk().unwrap();
            assert_eq!(p, &pmk, "same PMK as hostap");
            assert_eq!(i, &pmkid, "same PMKID as hostap");
            Some((pmk, pmkid))
        }
    }

    #[test]
    fn against_hostap_hunting_and_pecking() {
        for _ in 0..8 {
            assert!(run(false, b"correct horse battery", b"correct horse battery", false).is_some());
        }
        assert!(run(false, b"correct horse battery", b"incorrect horse", false).is_none());
    }

    #[test]
    fn against_hostap_hash_to_element() {
        for _ in 0..8 {
            assert!(run(true, b"correct horse battery", b"correct horse battery", false).is_some());
        }
        assert!(run(true, b"correct horse battery", b"incorrect horse", false).is_none());
    }

    #[test]
    fn against_hostap_with_an_anti_clogging_token() {
        assert!(run(false, b"a password", b"a password", true).is_some());
        assert!(run(true, b"a password", b"a password", true).is_some());
    }

    #[test]
    fn hostap_derives_the_same_pt_and_pwe() {
        // ref_new derives its PWE itself; our commit only verifies if the
        // password elements match, which the exchanges above already show
        // for random passwords of many lengths.
        for len in [1usize, 8, 31, 32, 63, 64, 100] {
            let pw: Vec<u8> = (0..len).map(|i| b'a' + (i % 26) as u8).collect();
            assert!(run(false, &pw, &pw, false).is_some(), "H&P length {len}");
            assert!(run(true, &pw, &pw, false).is_some(), "H2E length {len}");
        }
    }
}
