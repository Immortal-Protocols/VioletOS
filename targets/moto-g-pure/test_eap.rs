//! Host tests for the 802.1X supplicant core (eap.rs, eap_mschapv2.rs).
//!
//! `make test-unit` runs the first part: the old primitives against their
//! standards (RFC 1320, FIPS 180, RFC 2202, FIPS 46 and the worked examples
//! of RFC 2759 and RFC 3079), and the EAP state machine over a stand-in TLS
//! that only frames bytes, with a scripted authenticator.
//!
//! `targets/moto-g-pure/test_eap.sh hostap` builds this file again with
//! `--cfg hostap` and runs EAP-TLS and PEAP/MSCHAPv2 end to end: eap.rs over
//! BearSSL (eap_tls_bearssl.c) against hostap 2.11's EAP server on OpenSSL.
#![allow(dead_code)]
mod eap;

use eap::mschapv2 as ms;
use eap::{Eap, Step, Tls, PACKET_MAX};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
fn arr<const L: usize>(s: &str) -> [u8; L] {
    hex(s).try_into().unwrap()
}

#[test]
fn md4_sha1_hmac_and_des_match_their_standards() {
    assert_eq!(ms::md4(&[b""]).to_vec(), hex("31d6cfe0d16ae931b73c59d7e0c089c0"));
    assert_eq!(ms::md4(&[b"abc"]).to_vec(), hex("a448017aaf21d8525fc10ae87aa6729d"));
    assert_eq!(
        ms::md4(&[b"12345678901234567890123456789012345678901234567890", b"123456789012345678901234567890"]).to_vec(),
        hex("e33b4ddc9c38f2199c3e7b164fcc0536")
    );
    assert_eq!(ms::sha1(&[b"abc"]).to_vec(), hex("a9993e364706816aba3e25717850c26c9cd0d89d"));
    assert_eq!(
        ms::sha1(&[b"abcdbcdecdefdefgefghfghighijhijk", b"ijkljklmklmnlmnomnopnopq"]).to_vec(),
        hex("84983e441c3bd26ebaae4aa1f95129e5e54670f1")
    );
    assert_eq!(ms::hmac_sha1(&[0x0b; 20], &[b"Hi There"]).to_vec(), hex("b617318655057264e28bc0b6fb378c8ef146be00"));
    assert_eq!(
        ms::hmac_sha1(&[0xaa; 80], &[b"Test Using Larger Than Block-Size Key - Hash Key First"]).to_vec(),
        hex("aa4ae5e15272d00e95705637ce8a3b55ed402112")
    );
    assert_eq!(ms::des_encrypt(&arr("133457799bbcdff1"), &arr("0123456789abcdef")).to_vec(), hex("85e813540f0ab405"));
    assert_eq!(ms::des_encrypt(&arr("0101010101010101"), &arr("8000000000000000")).to_vec(), hex("95f8a5e5dd31d900"));
}

// RFC 2759 section 9.2 and RFC 3079 section 3.5.3.
const USER: &[u8] = b"User";
const PASSWORD: &[u8] = b"clientPass";
const AUTH_CHALLENGE: &str = "5b5d7c7d7b3f2f3e3c2c602132262628";
const PEER_CHALLENGE: &str = "21402324255e262a28295f2b3a337c7e";

#[test]
fn mschapv2_matches_rfc_2759_and_rfc_3079() {
    let hash = ms::nt_hash(PASSWORD).unwrap();
    assert_eq!(hash.to_vec(), hex("44ebba8d5312b8d611474411f56989ae"));
    let (auth, peer) = (arr(AUTH_CHALLENGE), arr(PEER_CHALLENGE));
    assert_eq!(ms::challenge_hash(&peer, &auth, USER).to_vec(), hex("d02e4386bce91226"));
    let nt = ms::nt_response(&auth, &peer, USER, &hash);
    assert_eq!(nt.to_vec(), hex("82309ecd8d708b5ea08faa3981cd83544233114a3d85d6df"));
    assert_eq!(ms::authenticator_response(&hash, &nt, &peer, &auth, USER).to_vec(), hex("407a5589115fd0d6209f510fe9c04566932cda56"));
    let master = ms::master_key(&hash, &nt);
    assert_eq!(master.to_vec(), hex("fdece3717a8c838cb388e527ae3cdd31"));
    // RFC 3079's SendStartKey128 is the server's send key: the client's receive key.
    assert_eq!(ms::start_key(&master, false).to_vec(), hex("8b7cdc149b993a1ba118cb153f56dccb"));
    assert_ne!(ms::start_key(&master, true), ms::start_key(&master, false));
    // The domain is not part of the hashed name; non-ASCII passwords are UTF-16.
    assert_eq!(ms::challenge_hash(&peer, &auth, b"CORP\\User"), ms::challenge_hash(&peer, &auth, USER));
    assert_eq!(ms::nt_hash("pässword".as_bytes()), Some(ms::md4(&[&[b'p', 0, 0xe4, 0, b's', 0, b's', 0, b'w', 0, b'o', 0, b'r', 0, b'd', 0]])));
    assert_eq!(ms::nt_hash(&[0xff, 0xfe]), None);
    assert_eq!(ms::nt_hash(&[b'a'; 65]), None);
}

/// A TLS that only frames: tag, two length bytes, payload. Server to client:
/// 1 hello flight, 2 finished, 3 application data, 9 a bad certificate.
/// Client to server: 0x10 hello, 0x11 second flight, 0x13 application data.
struct Fake {
    inbox: Vec<u8>,
    out: Vec<u8>,
    app: Vec<Vec<u8>>,
    up: bool,
    flight: usize,
    writes: usize,
}

impl Fake {
    fn new(flight: usize) -> Fake {
        let mut f = Fake { inbox: Vec::new(), out: Vec::new(), app: Vec::new(), up: false, flight, writes: 0 };
        f.record(0x10, &[0xc1; 200]);
        f
    }
    fn record(&mut self, tag: u8, payload: &[u8]) {
        self.out.push(tag);
        self.out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        self.out.extend_from_slice(payload);
    }
}

impl Tls for Fake {
    fn input(&mut self, data: &[u8]) -> bool {
        self.inbox.extend_from_slice(data);
        while self.inbox.len() >= 3 {
            let n = usize::from(u16::from_be_bytes([self.inbox[1], self.inbox[2]]));
            if self.inbox.len() < 3 + n {
                break;
            }
            let payload: Vec<u8> = self.inbox[3..3 + n].to_vec();
            match self.inbox[0] {
                1 => {
                    let flight = vec![0xc2; self.flight];
                    self.record(0x11, &flight);
                }
                2 => self.up = true,
                3 if self.up => self.app.push(payload),
                _ => return false,
            }
            self.inbox.drain(..3 + n);
        }
        true
    }
    fn output(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.out.len());
        out[..n].copy_from_slice(&self.out[..n]);
        self.out.drain(..n);
        n
    }
    fn established(&self) -> bool {
        self.up
    }
    fn export(&mut self, label: &[u8], out: &mut [u8]) -> bool {
        for (n, b) in out.iter_mut().enumerate() {
            *b = label[n % label.len()] ^ n as u8 ^ 0x5a;
        }
        self.up
    }
    fn write(&mut self, data: &[u8]) -> bool {
        if !self.up {
            return false;
        }
        self.writes += 1;
        self.record(0x13, data);
        true
    }
    fn read(&mut self, out: &mut [u8]) -> usize {
        if self.app.is_empty() {
            return 0;
        }
        let p = self.app.remove(0);
        out[..p.len()].copy_from_slice(&p);
        p.len()
    }
}

fn request(id: u8, kind: u8, data: &[u8]) -> Vec<u8> {
    let mut p = vec![1, id];
    p.extend_from_slice(&((5 + data.len()) as u16).to_be_bytes());
    p.push(kind);
    p.extend_from_slice(data);
    p
}
fn record(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut r = vec![tag];
    r.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    r.extend_from_slice(payload);
    r
}
const SUCCESS: [u8; 4] = [3, 0, 0, 4];
const FAILURE: [u8; 4] = [4, 0, 0, 4];

/// The authenticator's end of the scripted exchanges.
struct Peer {
    eap: Eap,
    tls: Fake,
    id: u8,
    kind: u8,
}

impl Peer {
    fn send(&mut self, packet: &[u8]) -> (Step, Vec<u8>) {
        let mut resp = [0u8; PACKET_MAX];
        let step = self.eap.receive(&mut self.tls, packet, &mut resp);
        let bytes = if let Step::Send(n) = step { resp[..n].to_vec() } else { Vec::new() };
        (step, bytes)
    }
    /// A request of the method's type; the response must be well formed.
    fn ask(&mut self, flags_and_data: &[u8]) -> Vec<u8> {
        self.id = self.id.wrapping_add(1);
        let (step, resp) = self.send(&request(self.id, self.kind, flags_and_data));
        assert!(matches!(step, Step::Send(_)), "{:?}", step);
        assert_eq!(resp[..2], [2, self.id]);
        assert_eq!(usize::from(u16::from_be_bytes([resp[2], resp[3]])), resp.len());
        assert_eq!(resp[4], self.kind);
        resp[5..].to_vec()
    }
    /// Send one whole TLS message; collect the peer's whole answer, acknowledging its fragments.
    fn message(&mut self, tls_bytes: &[u8]) -> Vec<u8> {
        let mut data = vec![0];
        data.extend_from_slice(tls_bytes);
        let mut resp = self.ask(&data);
        let mut whole = Vec::new();
        let mut stated = None;
        loop {
            let flags = resp[0];
            let mut body = &resp[1..];
            if flags & 0x80 != 0 {
                stated = Some(u32::from_be_bytes(body[..4].try_into().unwrap()) as usize);
                body = &body[4..];
            }
            whole.extend_from_slice(body);
            if flags & 0x40 == 0 {
                break;
            }
            resp = self.ask(&[0]);
        }
        if let Some(n) = stated {
            assert_eq!(n, whole.len(), "the stated length is the message's");
        }
        whole
    }
    fn start(&mut self) -> Vec<u8> {
        self.id = self.id.wrapping_add(1);
        let (_, resp) = self.send(&request(self.id, self.kind, &[0x20]));
        resp[6..].to_vec()
    }
    /// The inner EAP exchange: plaintext in, the peer's plaintext answer out.
    fn inner(&mut self, plain: &[u8]) -> Vec<u8> {
        let answer = self.message(&record(3, plain));
        if answer.is_empty() {
            return answer;
        }
        assert_eq!(answer[0], 0x13);
        answer[3..].to_vec()
    }
}

fn tls_peer(flight: usize) -> Peer {
    Peer { eap: Eap::tls(b"phone@violet.test").unwrap(), tls: Fake::new(flight), id: 0, kind: eap::TYPE_TLS }
}

#[test]
fn identity_notification_and_nak() {
    let mut p = tls_peer(100);
    let (step, resp) = p.send(&request(7, eap::TYPE_IDENTITY, b"hello"));
    assert_eq!(step, Step::Send(5 + 17));
    assert_eq!(resp, [&[2, 7, 0, 22, 1][..], b"phone@violet.test"].concat());
    assert_eq!(p.send(&request(8, eap::TYPE_NOTIFICATION, b"note")).1, [2, 8, 0, 5, 2]);
    // MD5-Challenge, or PEAP when the profile says EAP-TLS: "I do EAP-TLS".
    assert_eq!(p.send(&request(9, 4, &[16; 17])).1, [2, 9, 0, 6, 3, 13]);
    assert_eq!(p.send(&request(10, eap::TYPE_PEAP, &[0x20])).1, [2, 10, 0, 6, 3, 13]);
    // Malformed packets are dropped without a word.
    for bad in [&[1u8, 1, 0][..], &[1, 1, 0, 3], &[1, 1, 0, 9, 13], &[1, 1, 0, 4], &[9, 1, 0, 5, 1], &[2, 1, 0, 5, 1]] {
        assert_eq!(p.send(bad).0, Step::None);
    }
    // The EAPOL wrapping (802.1X): version, type, length, EAP packet.
    assert_eq!(eap::from_eapol(&[2, 0, 0, 5, 1, 7, 0, 5, 1, 0xee]), Some(&[1u8, 7, 0, 5, 1][..]), "padding after the stated length is dropped");
    assert_eq!(eap::from_eapol(&[2, 3, 0, 5, 1, 7, 0, 5, 1]), None, "an EAPOL-Key frame is not EAP");
    assert_eq!(eap::from_eapol(&[2, 0, 0, 6, 1, 7, 0, 5, 1]), None);
    assert_eq!(eap::from_eapol(&[2, 0, 0]), None);
    assert_eq!(eap::eapol_header(false, 22), [2, 0, 0, 22]);
    assert_eq!(eap::eapol_header(true, 22), [2, 1, 0, 0]);
    assert!(Eap::tls(b"").is_none() && Eap::tls(&[b'a'; 65]).is_none());
    assert!(Eap::peap(b"anonymous", b"user", b"pw", [0; 16], false).is_none(), "no peer challenge");
    assert!(Eap::peap(b"anonymous", b"", b"pw", [1; 16], false).is_none());
    assert!(Eap::peap(b"anonymous", b"user", &[0xff], [1; 16], false).is_none());
}

#[test]
fn eap_tls_with_fragments_both_ways() {
    let mut p = tls_peer(3000);
    assert_eq!(p.start(), record(0x10, &[0xc1; 200]), "the client hello answers the start");
    // The server's flight in three fragments: length + more, more, last.
    let flight = record(1, &[0x51; 2500]);
    let mut first = vec![0xc0];
    first.extend_from_slice(&(flight.len() as u32).to_be_bytes());
    first.extend_from_slice(&flight[..1000]);
    assert_eq!(p.ask(&first), [0], "an empty acknowledgement");
    assert_eq!(p.ask(&[&[0x40][..], &flight[1000..2000]].concat()), [0]);
    // A forged Success now is ignored and gives no key.
    assert_eq!(p.send(&SUCCESS).0, Step::None);
    assert!(p.eap.msk().is_none());
    // The last fragment; the client's 3000-byte flight comes back in three.
    let mut resp = p.ask(&[&[0][..], &flight[2000..]].concat());
    assert_eq!(resp[0], 0xc0);
    assert_eq!(resp[1..5], 3003u32.to_be_bytes());
    assert_eq!(resp.len(), 5 + 1390);
    let mut got = resp[5..].to_vec();
    let second = p.ask(&[0]);
    assert_eq!(second[0], 0x40, "more to come, no length");
    // The authenticator did not hear it and asks again: the same fragment.
    let dup = p.send(&request(p.id, eap::TYPE_TLS, &[0])).1;
    assert_eq!(dup[5..], second[..], "a repeated request gets the same response");
    got.extend_from_slice(&second[1..]);
    resp = p.ask(&[0]);
    assert_eq!(resp[0], 0);
    got.extend_from_slice(&resp[1..]);
    assert_eq!(got, record(0x11, &[0xc2; 3000]));
    assert!(p.eap.msk().is_none());
    // Finished: the handshake is complete, an empty response, then Success.
    assert_eq!(p.message(&record(2, b"")), b"");
    assert!(p.eap.msk().is_none(), "no key before the Success");
    assert_eq!(p.send(&SUCCESS).0, Step::Success);
    let mut want = [0u8; 64];
    Fake::new(0).up_export(&mut want);
    assert_eq!(p.eap.msk().unwrap(), &want);
    // Nothing more is answered after that.
    assert_eq!(p.send(&request(99, eap::TYPE_IDENTITY, b"")).0, Step::None);
}

impl Fake {
    fn up_export(&mut self, out: &mut [u8]) {
        self.up = true;
        assert!(self.export(b"client EAP encryption", out));
    }
}

#[test]
fn a_rejected_certificate_or_protocol_error_ends_the_attempt() {
    // Tag 9 is the stand-in for a certificate the TLS refuses.
    let mut p = tls_peer(100);
    p.start();
    p.id += 1;
    assert_eq!(p.send(&request(p.id, eap::TYPE_TLS, &[&[0][..], &record(9, b"bad")].concat())).0, Step::Failure);
    assert_eq!(p.send(&SUCCESS).0, Step::None, "no Success is believed afterwards");
    assert_eq!(p.send(&request(50, eap::TYPE_IDENTITY, b"")).0, Step::None);
    assert!(p.eap.msk().is_none());

    // Data before the start, a second start, data while we are mid-message.
    let mut p = tls_peer(100);
    assert_eq!(p.send(&request(1, eap::TYPE_TLS, &[&[0][..], &record(1, b"x")].concat())).0, Step::Failure);
    let mut p = tls_peer(100);
    p.start();
    assert_eq!(p.send(&request(5, eap::TYPE_TLS, &[0x20])).0, Step::Failure);
    let mut p = tls_peer(3000);
    p.start();
    let first = p.ask(&[&[0][..], &record(1, b"hello")].concat());
    assert_eq!(first[0], 0xc0);
    assert_eq!(p.send(&request(77, eap::TYPE_TLS, &[&[0][..], &record(2, b"")].concat())).0, Step::Failure);

    // A flight larger than the buffer; a length flag with no length; no flags byte.
    let mut p = tls_peer(eap::OUT_MAX);
    p.start();
    assert_eq!(p.send(&request(2, eap::TYPE_TLS, &[&[0][..], &record(1, b"hello")].concat())).0, Step::Failure);
    let mut p = tls_peer(100);
    p.start();
    assert_eq!(p.send(&request(2, eap::TYPE_TLS, &[0x80, 0, 0])).0, Step::Failure);
    let mut p = tls_peer(100);
    assert_eq!(p.send(&[1, 2, 0, 5, 13]).0, Step::Failure);

    // The authenticator's Failure always ends it, even after the method finished.
    let mut p = tls_peer(100);
    p.start();
    p.message(&record(1, b"hello"));
    p.message(&record(2, b""));
    assert_eq!(p.send(&FAILURE).0, Step::Failure);
    assert_eq!(p.send(&SUCCESS).0, Step::None);
    assert!(p.eap.msk().is_none());
}

const PEER: [u8; 16] = [0x21, 0x40, 0x23, 0x24, 0x25, 0x5e, 0x26, 0x2a, 0x28, 0x29, 0x5f, 0x2b, 0x3a, 0x33, 0x7c, 0x7e];

fn peap_peer(require_binding: bool) -> Peer {
    let eap = Eap::peap(b"anonymous@violet.test", b"CORP\\User", PASSWORD, PEER, require_binding).unwrap();
    let mut p = Peer { eap, tls: Fake::new(300), id: 0, kind: eap::TYPE_PEAP };
    // Version 1 offered; we answer 0.
    p.id += 1;
    let (_, resp) = p.send(&request(p.id, eap::TYPE_PEAP, &[0x21]));
    assert_eq!(resp[5], 0, "PEAPv0");
    p.message(&record(1, b"hello"));
    assert_eq!(p.tls.writes, 0);
    assert_eq!(p.message(&record(2, b"")), b"", "handshake done: an empty acknowledgement");
    p
}

/// The server's MSCHAPv2 challenge; returns the peer's NT response after checking the packet.
fn challenge(p: &mut Peer) -> [u8; 24] {
    // Inner identity: no EAP header inside PEAPv0.
    assert_eq!(p.inner(&[eap::TYPE_IDENTITY]), [&[eap::TYPE_IDENTITY][..], b"CORP\\User"].concat());
    let mut c = vec![eap::TYPE_MSCHAPV2, 1, 42, 0, 4 + 1 + 16 + 6, 16];
    c.extend_from_slice(&hex(AUTH_CHALLENGE));
    c.extend_from_slice(b"server");
    let r = p.inner(&c);
    assert_eq!(r[..6], [eap::TYPE_MSCHAPV2, 2, 42, 0, (4 + 1 + 49 + 9) as u8, 49]);
    assert_eq!(r[6..22], PEER);
    assert_eq!(r[22..30], [0; 8]);
    assert_eq!(r[54], 0);
    assert_eq!(&r[55..], b"CORP\\User");
    // The response is RFC 2759's for these challenges (the domain is not hashed).
    assert_eq!(r[30..54], hex("82309ecd8d708b5ea08faa3981cd83544233114a3d85d6df")[..]);
    r[30..54].try_into().unwrap()
}

fn success_message(proof: &str) -> Vec<u8> {
    let text = format!("S={} M=OK", proof);
    let mut m = vec![eap::TYPE_MSCHAPV2, 3, 42];
    m.extend_from_slice(&((4 + text.len()) as u16).to_be_bytes());
    m.extend_from_slice(text.as_bytes());
    m
}
const PROOF: &str = "407A5589115FD0D6209F510FE9C04566932CDA56";

/// The server's Result (and cryptobinding) as hostap's server builds them.
fn result_request(status: u8, binding: Option<(&[u8; 20], &[u8; 32])>) -> Vec<u8> {
    let mut t = vec![0x80, 3, 0, 2, 0, status];
    if let Some((cmk, nonce)) = binding {
        let mut b = vec![0, 12, 0, 56, 0, 0, 0, 0];
        b.extend_from_slice(nonce);
        let mut signed = b.clone();
        signed.extend_from_slice(&[0; 20]);
        signed.push(25);
        b.extend_from_slice(&ms::hmac_sha1(cmk, &[&signed]));
        t.extend_from_slice(&b);
    }
    let mut p = vec![1, 9];
    p.extend_from_slice(&((5 + t.len()) as u16).to_be_bytes());
    p.push(eap::TYPE_TLV);
    p.extend_from_slice(&t);
    p
}

/// IPMK and CMK as the server derives them (the same construction as hostap's
/// eap_server_peap.c, written out again here).
fn compound_keys(nt: &[u8; 24]) -> ([u8; 40], [u8; 20]) {
    let mut tk = [0u8; 64];
    Fake::new(0).up_export(&mut tk);
    let master = ms::master_key(&ms::nt_hash(PASSWORD).unwrap(), nt);
    let isk = [ms::start_key(&master, true), ms::start_key(&master, false)].concat();
    let seed = [&b"Inner Methods Compound Keys"[..], &isk].concat();
    let t1 = ms::hmac_sha1(&tk[..40], &[&seed, &[1, 0, 0]]);
    let t2 = ms::hmac_sha1(&tk[..40], &[&t1, &seed, &[2, 0, 0]]);
    let t3 = ms::hmac_sha1(&tk[..40], &[&t2, &seed, &[3, 0, 0]]);
    let all = [t1, t2, t3].concat();
    (all[..40].try_into().unwrap(), all[40..].try_into().unwrap())
}

#[test]
fn peap_mschapv2_with_cryptobinding() {
    let mut p = peap_peer(true);
    let nt = challenge(&mut p);
    assert_eq!(p.inner(&success_message(PROOF)), [eap::TYPE_MSCHAPV2, 3]);
    assert_eq!(p.send(&SUCCESS).0, Step::None, "not before the protected result");
    let (ipmk, cmk) = compound_keys(&nt);
    let nonce = [0x6e; 32];
    let r = p.inner(&result_request(1, Some((&cmk, &nonce))));
    // Result success, then our half of the binding over the same nonce.
    assert_eq!(r[..11], [2, 9, 0, 71, eap::TYPE_TLV, 0x80, 3, 0, 2, 0, 1]);
    assert_eq!(r[11..19], [0, 12, 0, 56, 0, 0, 0, 1]);
    assert_eq!(r[19..51], nonce);
    let mut signed = r[11..51].to_vec();
    signed.extend_from_slice(&[0; 20]);
    signed.push(25);
    assert_eq!(r[51..], ms::hmac_sha1(&cmk, &[&signed]));
    assert_eq!(p.send(&SUCCESS).0, Step::Success);
    assert!(p.eap.bound());
    // The session key is the compound one, not the tunnel's.
    let t1 = ms::hmac_sha1(&ipmk, &[b"Session Key Generating Function", &[0], &[1, 0, 0]]);
    let t2 = ms::hmac_sha1(&ipmk, &[&t1, b"Session Key Generating Function", &[0], &[2, 0, 0]]);
    let t3 = ms::hmac_sha1(&ipmk, &[&t2, b"Session Key Generating Function", &[0], &[3, 0, 0]]);
    let t4 = ms::hmac_sha1(&ipmk, &[&t3, b"Session Key Generating Function", &[0], &[4, 0, 0]]);
    assert_eq!(p.eap.msk().unwrap()[..], [t1, t2, t3, t4].concat()[..64]);
}

/// Every 0x13 record of one answer, as plaintexts.
fn records_of(mut answer: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while !answer.is_empty() {
        assert_eq!(answer[0], 0x13);
        let n = usize::from(u16::from_be_bytes([answer[1], answer[2]]));
        out.push(answer[3..3 + n].to_vec());
        answer = &answer[3 + n..];
    }
    out
}

#[test]
fn several_inner_messages_in_one_round_are_each_answered() {
    // Two records in one round: the identity, then the challenge.
    let mut p = peap_peer(false);
    let mut c = vec![eap::TYPE_MSCHAPV2, 1, 42, 0, 4 + 1 + 16 + 6, 16];
    c.extend_from_slice(&hex(AUTH_CHALLENGE));
    c.extend_from_slice(b"server");
    let both = p.message(&[record(3, &[eap::TYPE_IDENTITY]), record(3, &c)].concat());
    let answers = records_of(&both);
    assert_eq!(answers.len(), 2, "both messages answered");
    assert_eq!(answers[0], [&[eap::TYPE_IDENTITY][..], b"CORP\\User"].concat());
    assert_eq!(answers[1][..3], [eap::TYPE_MSCHAPV2, 2, 42]);
    // Two messages run together in one record: the MSCHAPv2 Success and the
    // Result, each cut out by its own stated length.
    let merged = [success_message(PROOF), result_request(1, None)].concat();
    let answers = records_of(&p.message(&record(3, &merged)));
    assert_eq!(answers, [vec![eap::TYPE_MSCHAPV2, 3], vec![2, 9, 0, 11, eap::TYPE_TLV, 0x80, 3, 0, 2, 0, 1]]);
    assert_eq!(p.send(&SUCCESS).0, Step::Success);

    // A stated MSCHAPv2 length past the end of what came ends the attempt.
    let mut p = peap_peer(false);
    challenge(&mut p);
    let mut cut = success_message(PROOF);
    cut[4] += 1;
    p.id += 1;
    assert_eq!(p.send(&request(p.id, eap::TYPE_PEAP, &[&[0][..], &record(3, &cut)].concat())).0, Step::Failure);
    // So does a Result whose stated length runs past the record.
    let mut p = peap_peer(false);
    challenge(&mut p);
    let mut long = [success_message(PROOF), result_request(1, None)].concat();
    let at = success_message(PROOF).len() + 3;
    long[at] += 1;
    p.id += 1;
    assert_eq!(p.send(&request(p.id, eap::TYPE_PEAP, &[&[0][..], &record(3, &long)].concat())).0, Step::Failure);
    assert!(p.eap.msk().is_none());
}

#[test]
fn peap_without_binding_only_when_allowed() {
    let mut p = peap_peer(false);
    challenge(&mut p);
    p.inner(&success_message(PROOF));
    assert_eq!(p.inner(&result_request(1, None)), [2, 9, 0, 11, eap::TYPE_TLV, 0x80, 3, 0, 2, 0, 1]);
    assert_eq!(p.send(&SUCCESS).0, Step::Success);
    assert!(!p.eap.bound());
    let mut tunnel = [0u8; 64];
    Fake::new(0).up_export(&mut tunnel);
    assert_eq!(p.eap.msk().unwrap(), &tunnel);

    // A profile that demands the binding refuses a server without it.
    let mut p = peap_peer(true);
    challenge(&mut p);
    p.inner(&success_message(PROOF));
    assert_eq!(p.inner(&result_request(1, None))[10], 2, "Result: failure");
    assert_eq!(p.send(&SUCCESS).0, Step::None);
    assert!(p.eap.msk().is_none());
}

#[test]
fn peap_refuses_a_server_that_cannot_prove_the_password() {
    // A wrong authenticator response: whoever ends this tunnel does not know the password.
    let mut p = peap_peer(false);
    challenge(&mut p);
    p.id += 1;
    let bad = success_message("407A5589115FD0D6209F510FE9C04566932CDA57");
    assert_eq!(p.send(&request(p.id, eap::TYPE_PEAP, &[&[0][..], &record(3, &bad)].concat())).0, Step::Failure);
    assert_eq!(p.send(&SUCCESS).0, Step::None);
    assert!(p.eap.msk().is_none());

    // Result "success" with no inner method at all, or straight after the challenge.
    let mut p = peap_peer(false);
    assert_eq!(p.inner(&result_request(1, None))[10], 2);
    assert_eq!(p.send(&SUCCESS).0, Step::None);
    let mut p = peap_peer(false);
    challenge(&mut p);
    assert_eq!(p.inner(&result_request(1, None))[10], 2);
    assert_eq!(p.send(&SUCCESS).0, Step::None);
    assert!(p.eap.msk().is_none());

    // A binding computed with the wrong key (a tunnel that ends somewhere else).
    let mut p = peap_peer(false);
    challenge(&mut p);
    p.inner(&success_message(PROOF));
    assert_eq!(p.inner(&result_request(1, Some((&[7; 20], &[1; 32]))))[10], 2);
    assert_eq!(p.send(&SUCCESS).0, Step::None);

    // The server refuses the account: acknowledged, and nothing succeeds after.
    let mut p = peap_peer(false);
    challenge(&mut p);
    assert_eq!(p.inner(&[eap::TYPE_MSCHAPV2, 4, 42, 0, 8, b'E', b'=', b'6', b'9']), [eap::TYPE_MSCHAPV2, 4]);
    assert_eq!(p.inner(&result_request(2, None))[10], 2);
    assert_eq!(p.send(&SUCCESS).0, Step::None);
    assert_eq!(p.send(&FAILURE).0, Step::Failure);

    // A second challenge (fishing for another response) and other inner methods.
    let mut p = peap_peer(false);
    challenge(&mut p);
    p.id += 1;
    let mut c = vec![eap::TYPE_MSCHAPV2, 1, 43, 0, 21, 16];
    c.extend_from_slice(&[9; 16]);
    assert_eq!(p.send(&request(p.id, eap::TYPE_PEAP, &[&[0][..], &record(3, &c)].concat())).0, Step::Failure);
    let mut p = peap_peer(false);
    assert_eq!(p.inner(&[6, b'p', b'w', b'?']), [eap::TYPE_NAK, eap::TYPE_MSCHAPV2], "GTC would take the password itself");
    // A mandatory TLV we do not know.
    let mut p = peap_peer(false);
    p.id += 1;
    let odd = [1, 9, 0, 9, eap::TYPE_TLV, 0x80, 99, 0, 0];
    assert_eq!(p.send(&request(p.id, eap::TYPE_PEAP, &[&[0][..], &record(3, &odd)].concat())).0, Step::Failure);
}

#[cfg(hostap)]
mod hostap {
    use super::*;
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::sync::Mutex;

    extern "C" {
        fn ref_new(method: c_int, ca: *const c_char, cert: *const c_char, key: *const c_char, user: *const c_char, password: *const c_char, fragment: c_int) -> *mut c_void;
        fn ref_step(r: *mut c_void, resp: *const u8, resp_len: usize, out: *mut u8, max: usize, state: *mut c_int) -> usize;
        fn ref_key(r: *mut c_void, out: *mut u8) -> usize;
        fn ref_free(r: *mut c_void);
        fn eap_tls_size() -> usize;
        fn eap_tls_open(mem: *mut c_void, ca: *const u8, ca_len: usize, name: *const c_char, cert: *const u8, cert_len: usize, key: *const u8, key_len: usize, entropy: *const u8, entropy_len: usize, days: u32, seconds: u32) -> c_int;
        fn eap_tls_input(mem: *mut c_void, data: *const u8, len: usize) -> c_int;
        fn eap_tls_output(mem: *mut c_void, out: *mut u8, max: usize) -> usize;
        fn eap_tls_established(mem: *mut c_void) -> c_int;
        fn eap_tls_error(mem: *mut c_void) -> c_int;
        fn eap_tls_export(mem: *mut c_void, label: *const u8, label_len: usize, out: *mut u8, len: usize) -> c_int;
        fn eap_tls_write(mem: *mut c_void, data: *const u8, len: usize) -> c_int;
        fn eap_tls_read(mem: *mut c_void, out: *mut u8, max: usize) -> usize;
    }

    /// hostap's OpenSSL wrapper keeps global state: one exchange at a time.
    static ONE: Mutex<()> = Mutex::new(());

    fn dir() -> String {
        std::env::var("EAP_TEST_DIR").expect("EAP_TEST_DIR (test_eap.sh sets it)")
    }
    fn file(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/{}", dir(), name)).unwrap()
    }
    fn path(name: &str) -> CString {
        CString::new(format!("{}/{}", dir(), name)).unwrap()
    }
    fn now() -> (u32, u32) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        ((t / 86400) as u32 + 719_528, (t % 86400) as u32)
    }

    /// eap_tls_bearssl.c behind the trait, counting what goes into the tunnel.
    struct Bear {
        mem: Vec<u64>,
        writes: usize,
        _keep: (Vec<u8>, Vec<u8>, Vec<u8>),
    }
    impl Bear {
        fn open(ca: &str, name: &str, client: Option<(&str, &str)>, when: (u32, u32)) -> Option<Bear> {
            let ca = file(ca);
            let (cert, key) = client.map_or((Vec::new(), Vec::new()), |(c, k)| (file(c), file(k)));
            let mut mem = vec![0u64; unsafe { eap_tls_size() } / 8 + 1];
            let entropy: Vec<u8> = file_random();
            let name = CString::new(name).unwrap();
            let rc = unsafe {
                eap_tls_open(
                    mem.as_mut_ptr().cast(),
                    ca.as_ptr(),
                    ca.len(),
                    name.as_ptr(),
                    if client.is_some() { cert.as_ptr() } else { std::ptr::null() },
                    cert.len(),
                    if client.is_some() { key.as_ptr() } else { std::ptr::null() },
                    key.len(),
                    entropy.as_ptr(),
                    entropy.len(),
                    when.0,
                    when.1,
                )
            };
            (rc == 0).then_some(Bear { mem, writes: 0, _keep: (ca, cert, key) })
        }
        fn p(&mut self) -> *mut c_void {
            self.mem.as_mut_ptr().cast()
        }
    }
    fn file_random() -> Vec<u8> {
        use std::io::Read;
        let mut b = vec![0u8; 32];
        std::fs::File::open("/dev/urandom").unwrap().read_exact(&mut b).unwrap();
        b
    }
    impl Tls for Bear {
        fn input(&mut self, data: &[u8]) -> bool {
            unsafe { eap_tls_input(self.p(), data.as_ptr(), data.len()) == 1 }
        }
        fn output(&mut self, out: &mut [u8]) -> usize {
            unsafe { eap_tls_output(self.p(), out.as_mut_ptr(), out.len()) }
        }
        fn established(&self) -> bool {
            unsafe { eap_tls_established(self.mem.as_ptr() as *mut c_void) == 1 }
        }
        fn export(&mut self, label: &[u8], out: &mut [u8]) -> bool {
            unsafe { eap_tls_export(self.p(), label.as_ptr(), label.len(), out.as_mut_ptr(), out.len()) == 1 }
        }
        fn write(&mut self, data: &[u8]) -> bool {
            self.writes += 1;
            unsafe { eap_tls_write(self.p(), data.as_ptr(), data.len()) == 1 }
        }
        fn read(&mut self, out: &mut [u8]) -> usize {
            unsafe { eap_tls_read(self.p(), out.as_mut_ptr(), out.len()) }
        }
    }

    struct Server(*mut c_void);
    impl Server {
        fn new(method: i32, user: &str, password: &str, fragment: i32) -> Server {
            let (ca, cert, key) = (path("ca.pem"), path("server.pem"), path("server.key"));
            let (user, password) = (CString::new(user).unwrap(), CString::new(password).unwrap());
            let r = unsafe { ref_new(method, ca.as_ptr(), cert.as_ptr(), key.as_ptr(), user.as_ptr(), password.as_ptr(), fragment) };
            assert!(!r.is_null(), "hostap's EAP server did not start");
            Server(r)
        }
        fn step(&mut self, resp: Option<&[u8]>) -> (Vec<u8>, i32) {
            let mut out = vec![0u8; 4096];
            let mut state = 0;
            let n = unsafe { ref_step(self.0, resp.map_or(std::ptr::null(), |r| r.as_ptr()), resp.map_or(0, |r| r.len()), out.as_mut_ptr(), out.len(), &mut state) };
            out.truncate(n);
            (out, state)
        }
        fn key(&mut self) -> Option<[u8; 64]> {
            let mut k = [0u8; 64];
            (unsafe { ref_key(self.0, k.as_mut_ptr()) } == 64).then_some(k)
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            unsafe { ref_free(self.0) }
        }
    }

    #[derive(Debug, PartialEq)]
    struct Outcome {
        last: Step,
        server: i32,
        rounds: usize,
        fragments: usize,
    }

    /// Run the exchange to its end. `forge` may slip a packet in before round N.
    fn run(eap: &mut Eap, tls: &mut Bear, server: &mut Server, forge: Option<(usize, &[u8])>) -> Outcome {
        let (mut packet, mut state) = server.step(None);
        let mut o = Outcome { last: Step::None, server: 0, rounds: 0, fragments: 0 };
        let mut resp = [0u8; PACKET_MAX];
        while o.rounds < 200 {
            o.rounds += 1;
            if let Some((at, forged)) = forge {
                if at == o.rounds {
                    assert_eq!(eap.receive(tls, forged, &mut resp), Step::None, "a forged packet is ignored");
                    assert!(eap.msk().is_none());
                }
            }
            o.server = state;
            if packet.is_empty() {
                break;
            }
            o.last = eap.receive(tls, &packet, &mut resp);
            let Step::Send(n) = o.last else { break };
            if n > 5 && resp[5] & 0x40 != 0 {
                o.fragments += 1;
            }
            (packet, state) = server.step(Some(&resp[..n]));
        }
        o
    }

    #[test]
    fn peap_mschapv2_against_hostap() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        for (server_fragment, client_fragment) in [(1398, 1390), (300, 120)] {
            let mut server = Server::new(25, "violet-user", "correct horse battery", server_fragment);
            let mut tls = Bear::open("ca.der", "radius.violet.test", None, now()).unwrap();
            let mut eap = Eap::peap(b"anonymous@violet.test", b"violet-user", b"correct horse battery", [7; 16], true).unwrap();
            eap.set_fragment(client_fragment);
            let o = run(&mut eap, &mut tls, &mut server, Some((4, &SUCCESS)));
            assert_eq!((o.last, o.server), (Step::Success, 1), "{:?} (BearSSL error {})", o, unsafe { eap_tls_error(tls.p()) });
            assert!(eap.bound(), "hostap's server offers the cryptobinding and we take it");
            assert_eq!(eap.msk().unwrap()[..], server.key().expect("the server's MSK")[..], "both ends hold the same MSK");
            assert!(tls.writes >= 3, "identity, MSCHAPv2 response, MSCHAPv2 success, result");
            if client_fragment < 200 {
                assert!(o.fragments > 0, "the client hello went out in pieces");
            }
        }
    }

    #[test]
    fn peap_wrong_password_fails_at_both_ends() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let mut server = Server::new(25, "violet-user", "correct horse battery", 1398);
        let mut tls = Bear::open("ca.der", "radius.violet.test", None, now()).unwrap();
        let mut eap = Eap::peap(b"anonymous", b"violet-user", b"wrong", [7; 16], false).unwrap();
        let o = run(&mut eap, &mut tls, &mut server, None);
        assert_eq!((o.last, o.server), (Step::Failure, 2), "{:?}", o);
        assert!(eap.msk().is_none() && server.key().is_none());
    }

    /// The server's certificate is not from the profile's authority, has the
    /// wrong name, or is outside its dates: the handshake dies and nothing
    /// (no identity, no MSCHAPv2 response) ever enters the tunnel.
    #[test]
    fn peap_untrusted_server_gets_no_credentials() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let far = (now().0 + 365 * 30, 0);
        for (ca, name, when) in [("other-ca.der", "radius.violet.test", now()), ("ca.der", "radius.evil.test", now()), ("ca.der", "radius.violet.test", far)] {
            let mut server = Server::new(25, "violet-user", "correct horse battery", 1398);
            let mut tls = Bear::open(ca, name, None, when).unwrap();
            let mut eap = Eap::peap(b"anonymous", b"violet-user", b"correct horse battery", [7; 16], false).unwrap();
            let o = run(&mut eap, &mut tls, &mut server, None);
            assert_eq!(o.last, Step::Failure, "{} {}: {:?}", ca, name, o);
            assert_eq!(tls.writes, 0, "{} {}", ca, name);
            assert!(eap.msk().is_none() && server.key().is_none());
            let mut resp = [0u8; PACKET_MAX];
            assert_eq!(eap.receive(&mut tls, &SUCCESS, &mut resp), Step::None);
        }
        // No authority, no name or no clock: no session at all.
        assert!(Bear::open("ca.der", "", None, now()).is_none());
        assert!(Bear::open("ca.der", "radius.violet.test", None, (0, 0)).is_none());
        assert!(Bear::open("server.der", "radius.violet.test", None, now()).is_none(), "a leaf certificate is not an authority");
    }

    #[test]
    fn eap_tls_against_hostap() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        for (cert, key) in [("client-ec.der", "client-ec.key.der"), ("client-rsa.der", "client-rsa.key.der")] {
            for (server_fragment, client_fragment) in [(1398, 1390), (250, 100)] {
                let mut server = Server::new(13, "", "", server_fragment);
                let mut tls = Bear::open("ca.der", "radius.violet.test", Some((cert, key)), now()).unwrap();
                let mut eap = Eap::tls(b"phone@violet.test").unwrap();
                eap.set_fragment(client_fragment);
                let o = run(&mut eap, &mut tls, &mut server, Some((3, &SUCCESS)));
                assert_eq!((o.last, o.server), (Step::Success, 1), "{} {:?} (BearSSL error {})", cert, o, unsafe { eap_tls_error(tls.p()) });
                assert_eq!(eap.msk().unwrap()[..], server.key().expect("the server's MSK")[..]);
                assert!(o.fragments > 0, "the certificate flight went out in pieces");
            }
        }
    }

    #[test]
    fn eap_tls_needs_a_certificate_the_server_trusts() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        // None at all, then one from another authority.
        for client in [None, Some(("stranger.der", "stranger.key.der"))] {
            let mut server = Server::new(13, "", "", 1398);
            let mut tls = Bear::open("ca.der", "radius.violet.test", client, now()).unwrap();
            let mut eap = Eap::tls(b"phone@violet.test").unwrap();
            let o = run(&mut eap, &mut tls, &mut server, None);
            assert_ne!(o.last, Step::Success, "{:?}", o);
            assert!(eap.msk().is_none() && server.key().is_none());
        }
        // And the phone refuses a server from another authority.
        let mut server = Server::new(13, "", "", 1398);
        let mut tls = Bear::open("other-ca.der", "radius.violet.test", Some(("client-ec.der", "client-ec.key.der")), now()).unwrap();
        let mut eap = Eap::tls(b"phone@violet.test").unwrap();
        assert_eq!(run(&mut eap, &mut tls, &mut server, None).last, Step::Failure);
    }
}
