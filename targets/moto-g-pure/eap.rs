//! The supplicant's side of 802.1X authentication (WPA2/WPA3-Enterprise):
//! EAP (RFC 3748) with EAP-TLS (RFC 5216) and PEAPv0 carrying EAP-MSCHAPv2.
//!
//! This is the protocol core only. It takes whole EAP packets (the body of an
//! EAPOL-EAP frame) and writes the responses; TLS itself is behind the `Tls`
//! trait (eap_tls_bearssl.c is the BearSSL one). It is NOT linked into Radio
//! yet: see docs in targets/moto-g-pure/README.md, "Enterprise Wi-Fi".
//!
//! What the core guarantees, whatever the server or an attacker sends:
//! - EAP-Success counts only after the method finished: for EAP-TLS, the TLS
//!   handshake completed (so the `Tls` checked the server's certificate and
//!   name); for PEAP, also the server proved inside the tunnel that it knows
//!   the password (MSCHAPv2 authenticator response) and sent a successful
//!   Result. A Success before that is ignored.
//! - The password is only ever used inside the established tunnel.
//! - `msk()` gives nothing before that Success.
//! - A malformed, out-of-order or oversized method message (EAP-TLS/PEAP
//!   flags and fragments, the TLS records, the inner PEAP messages) ends the
//!   attempt. An outer EAP packet that cannot be parsed (stated length under
//!   4 or past the frame, a Request with no type, an unknown code) is
//!   silently discarded, as RFC 3748 4.1 asks, and the attempt goes on; an
//!   inner method other than MSCHAPv2 gets a Nak.
//!
//! Not here: session resumption, PEAPv1, EAP-TTLS, password change, TLS 1.3
//! key labels (BearSSL speaks TLS 1.0-1.2).
//!
//! Pure `core` code. Host tests: test_eap.rs (`make test-unit`), and against
//! hostap's EAP server with `targets/moto-g-pure/test_eap.sh hostap`.
#[path = "eap_mschapv2.rs"]
pub mod mschapv2;

use mschapv2::{hmac_sha1, same};

/// A TLS client session. `eap` never sees keys except through `export`.
pub trait Tls {
    /// Bytes from the server, in any split. False: the session is dead (bad
    /// certificate, alert, decode error); nothing more may be trusted.
    fn input(&mut self, data: &[u8]) -> bool;
    /// Records waiting to go to the server; 0 when there are none.
    fn output(&mut self, out: &mut [u8]) -> usize;
    /// The handshake finished and the server's certificate chain and name
    /// were accepted.
    fn established(&self) -> bool;
    /// RFC 5705 key material for `label` with no context.
    fn export(&mut self, label: &[u8], out: &mut [u8]) -> bool;
    /// Send application data in the tunnel.
    fn write(&mut self, data: &[u8]) -> bool;
    /// Received application data, one TLS record's plaintext per call (so
    /// two inner messages in two records never run together); 0 when there
    /// is none.
    fn read(&mut self, out: &mut [u8]) -> usize;
}

pub const TYPE_IDENTITY: u8 = 1;
pub const TYPE_NOTIFICATION: u8 = 2;
pub const TYPE_NAK: u8 = 3;
pub const TYPE_TLS: u8 = 13;
pub const TYPE_PEAP: u8 = 25;
pub const TYPE_MSCHAPV2: u8 = 26;
pub const TYPE_TLV: u8 = 33;

const REQUEST: u8 = 1;
const RESPONSE: u8 = 2;
const SUCCESS: u8 = 3;
const FAILURE: u8 = 4;
const FLAG_LENGTH: u8 = 0x80;
const FLAG_MORE: u8 = 0x40;
const FLAG_START: u8 = 0x20;

/// The largest TLS flight this side sends (a client certificate chain fits).
pub const OUT_MAX: usize = 4096;
/// The largest EAP packet this side sends.
pub const PACKET_MAX: usize = 1400;
pub const NAME_MAX: usize = 64;
const KEY_LABEL: &[u8] = b"client EAP encryption";

/// How long the first PEAPv0 inner message in `p` is. A TLV exchange (and
/// an identity request sent with its header) states its length in the EAP
/// header, MSCHAPv2 in its own; an identity request without a header, or a
/// method we refuse, says nothing and takes the rest. 0: a stated length
/// that does not fit, which ends the attempt.
fn inner_len(p: &[u8]) -> usize {
    let stated = |hi: usize, extra: usize| {
        let len = extra + usize::from(u16::from_be_bytes([p[hi], p[hi + 1]]));
        if (5..=p.len()).contains(&len) { len } else { 0 }
    };
    if p.len() >= 5 && p[0] == REQUEST && (p[4] == TYPE_TLV || p[2..4] == [0, 5] && p[4] == TYPE_IDENTITY) {
        return stated(2, 0);
    }
    if p.len() >= 5 && p[0] == TYPE_MSCHAPV2 {
        return stated(3, 1);
    }
    p.len()
}

/// The EAP packet in an EAPOL frame (the 802.1X payload of ethertype 888E),
/// if the frame is an EAP-Packet with a consistent length.
pub fn from_eapol(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() < 4 || frame[1] != 0 {
        return None;
    }
    let n = usize::from(u16::from_be_bytes([frame[2], frame[3]]));
    frame.get(4..4 + n)
}

/// The four-byte EAPOL header (802.1X-2004, version 2) before an EAP packet
/// of `len` bytes; with `start`, the EAPOL-Start that asks the access point
/// to begin (no body).
pub fn eapol_header(start: bool, len: usize) -> [u8; 4] {
    let n = if start { 0 } else { len as u16 };
    [2, u8::from(start), (n >> 8) as u8, n as u8]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// EAP-TLS: the client certificate is the credential (held by the `Tls`).
    Tls,
    /// PEAPv0 with EAP-MSCHAPv2 inside.
    Peap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing to send (the packet was ignored).
    None,
    /// Send this many bytes of the output buffer.
    Send(usize),
    /// The server's EAP-Success, after the method finished: `msk()` is ready.
    Success,
    /// The attempt is over without keys.
    Failure,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Inner {
    Idle,
    /// Our MSCHAPv2 response is out; waiting for the server's proof.
    Answered,
    /// The server proved it knows the password.
    Proven,
    Rejected,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Nothing,
    /// Identity, Nak, Notification: rebuilt from the request.
    Plain,
    /// The fragment out[from..pos].
    Fragment,
    Ack,
}

#[derive(Clone, Copy)]
struct Text {
    bytes: [u8; NAME_MAX],
    len: u8,
}

impl Text {
    fn new(b: &[u8]) -> Option<Text> {
        if b.len() > NAME_MAX {
            return None;
        }
        let mut t = Text { bytes: [0; NAME_MAX], len: b.len() as u8 };
        t.bytes[..b.len()].copy_from_slice(b);
        Some(t)
    }
    fn get(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

pub struct Eap {
    method: Method,
    /// The outer identity (may be anonymous for PEAP).
    identity: Text,
    /// The account inside the tunnel.
    user: Text,
    password: Text,
    peer_challenge: [u8; 16],
    /// Refuse a PEAP server that does not bind the tunnel to the inner method.
    require_binding: bool,
    fragment: usize,

    started: bool,
    failed: bool,
    /// The method finished with the server authenticated.
    done: bool,
    accepted: bool,
    last_id: u16,
    last: Last,
    out: [u8; OUT_MAX],
    out_len: usize,
    out_from: usize,
    out_pos: usize,

    inner: Inner,
    proof: [u8; 20],
    master: [u8; 16],
    binding_used: bool,
    ipmk: [u8; 40],
    key: [u8; 64],
}

fn wipe(b: &mut [u8]) {
    for x in b {
        unsafe { core::ptr::write_volatile(x, 0) };
    }
}

impl Drop for Eap {
    fn drop(&mut self) {
        wipe(&mut self.password.bytes);
        wipe(&mut self.proof);
        wipe(&mut self.master);
        wipe(&mut self.ipmk);
        wipe(&mut self.key);
        wipe(&mut self.out);
    }
}

/// PEAPv0's PRF+ (draft-josefsson-pppext-eap-tls-eap-06, as deployed):
/// T1 = HMAC-SHA1(K, S | 01 00 00), Tn = HMAC-SHA1(K, Tn-1 | S | n 00 00).
fn prf_plus(key: &[u8], label: &[u8], seed: &[u8], out: &mut [u8]) {
    let mut t = [0u8; 20];
    let mut have = 0;
    let mut counter = 0u8;
    for chunk in out.chunks_mut(20) {
        counter += 1;
        t = hmac_sha1(key, &[&t[..have], label, seed, &[counter, 0, 0]]);
        have = 20;
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
    wipe(&mut t);
}

impl Eap {
    /// EAP-TLS as `identity`. The certificate and key live in the `Tls`.
    pub fn tls(identity: &[u8]) -> Option<Eap> {
        Eap::new(Method::Tls, identity, b"", b"", [0; 16], false)
    }

    /// PEAPv0/MSCHAPv2. `outer` is what goes out before the tunnel exists
    /// (use "anonymous" or "anonymous@realm" to keep the account private);
    /// `peer_challenge` must be 16 fresh random bytes.
    pub fn peap(outer: &[u8], user: &[u8], password: &[u8], peer_challenge: [u8; 16], require_binding: bool) -> Option<Eap> {
        if user.is_empty() || peer_challenge == [0; 16] || mschapv2::nt_hash(password).is_none() {
            return None;
        }
        Eap::new(Method::Peap, outer, user, password, peer_challenge, require_binding)
    }

    fn new(method: Method, identity: &[u8], user: &[u8], password: &[u8], peer_challenge: [u8; 16], require_binding: bool) -> Option<Eap> {
        if identity.is_empty() {
            return None;
        }
        Some(Eap {
            method,
            identity: Text::new(identity)?,
            user: Text::new(user)?,
            password: Text::new(password)?,
            peer_challenge,
            require_binding,
            fragment: PACKET_MAX - 10,
            started: false,
            failed: false,
            done: false,
            accepted: false,
            last_id: 0xffff,
            last: Last::Nothing,
            out: [0; OUT_MAX],
            out_len: 0,
            out_from: 0,
            out_pos: 0,
            inner: Inner::Idle,
            proof: [0; 20],
            master: [0; 16],
            binding_used: false,
            ipmk: [0; 40],
            key: [0; 64],
        })
    }

    /// The most TLS bytes per EAP packet (for links with a small MTU).
    pub fn set_fragment(&mut self, bytes: usize) {
        self.fragment = bytes.clamp(64, PACKET_MAX - 10);
    }

    fn method_type(&self) -> u8 {
        match self.method {
            Method::Tls => TYPE_TLS,
            Method::Peap => TYPE_PEAP,
        }
    }

    /// The 64-byte MSK, once the server's EAP-Success followed a finished
    /// method. The pairwise master key is its first 32 bytes.
    pub fn msk(&self) -> Option<&[u8; 64]> {
        (self.accepted && !self.failed).then_some(&self.key)
    }

    /// Did PEAP bind the tunnel to the inner method (cryptobinding)?
    pub fn bound(&self) -> bool {
        self.binding_used
    }

    fn fail(&mut self) -> Step {
        self.failed = true;
        self.done = false;
        self.accepted = false;
        wipe(&mut self.key);
        Step::Failure
    }

    /// One EAP packet from the authenticator; a response, if any, goes to `resp`
    /// (at least PACKET_MAX bytes).
    pub fn receive(&mut self, tls: &mut dyn Tls, packet: &[u8], resp: &mut [u8]) -> Step {
        if packet.len() < 4 || resp.len() < PACKET_MAX {
            return Step::None;
        }
        let len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
        if len < 4 || len > packet.len() {
            return Step::None;
        }
        let (code, id, body) = (packet[0], packet[1], &packet[4..len]);
        match code {
            SUCCESS if self.done && !self.failed => {
                self.accepted = true;
                Step::Success
            }
            // A Success nobody earned (forged, or a server skipping the method).
            SUCCESS => Step::None,
            FAILURE => self.fail(),
            REQUEST if self.failed || self.accepted || body.is_empty() => Step::None,
            REQUEST => self.request(tls, id, body[0], &body[1..], resp),
            _ => Step::None,
        }
    }

    fn plain(&mut self, id: u8, kind: u8, data: &[u8], resp: &mut [u8]) -> Step {
        self.last_id = u16::from(id);
        self.last = Last::Plain;
        let len = 5 + data.len();
        resp[0] = RESPONSE;
        resp[1] = id;
        resp[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        resp[4] = kind;
        resp[5..len].copy_from_slice(data);
        Step::Send(len)
    }

    fn request(&mut self, tls: &mut dyn Tls, id: u8, kind: u8, data: &[u8], resp: &mut [u8]) -> Step {
        let method = self.method_type();
        match kind {
            TYPE_IDENTITY => {
                let identity = self.identity;
                self.plain(id, TYPE_IDENTITY, identity.get(), resp)
            }
            TYPE_NOTIFICATION => self.plain(id, TYPE_NOTIFICATION, &[], resp),
            k if k == method => self.tunnel(tls, id, data, resp),
            // Any other method: say which one we do.
            _ => self.plain(id, TYPE_NAK, &[method], resp),
        }
    }

    /// The next piece of out[] as an EAP-TLS/PEAP response (or an empty one).
    fn emit(&mut self, id: u8, resp: &mut [u8]) -> Step {
        self.last_id = u16::from(id);
        let left = self.out_len - self.out_pos;
        resp[0] = RESPONSE;
        resp[1] = id;
        resp[4] = self.method_type();
        if left == 0 {
            self.last = Last::Ack;
            resp[2..4].copy_from_slice(&6u16.to_be_bytes());
            resp[5] = 0;
            return Step::Send(6);
        }
        self.last = Last::Fragment;
        self.out_from = self.out_pos;
        let take = left.min(self.fragment);
        let mut flags = 0;
        let mut at = 6;
        if self.out_pos == 0 && take < left {
            // The first of several: say how long the whole message is.
            flags |= FLAG_LENGTH;
            resp[6..10].copy_from_slice(&(self.out_len as u32).to_be_bytes());
            at = 10;
        }
        if take < left {
            flags |= FLAG_MORE;
        }
        resp[5] = flags;
        resp[at..at + take].copy_from_slice(&self.out[self.out_pos..self.out_pos + take]);
        self.out_pos += take;
        resp[2..4].copy_from_slice(&((at + take) as u16).to_be_bytes());
        Step::Send(at + take)
    }

    fn collect(&mut self, tls: &mut dyn Tls) -> bool {
        loop {
            if self.out_len == OUT_MAX {
                // Is there more than fits?
                let mut probe = [0u8; 1];
                return tls.output(&mut probe) == 0;
            }
            let n = tls.output(&mut self.out[self.out_len..]);
            if n == 0 {
                return true;
            }
            self.out_len += n;
        }
    }

    fn tunnel(&mut self, tls: &mut dyn Tls, id: u8, data: &[u8], resp: &mut [u8]) -> Step {
        if u16::from(id) == self.last_id {
            // The authenticator did not get our answer: the same one again.
            match self.last {
                Last::Fragment => {
                    self.out_pos = self.out_from;
                    return self.emit(id, resp);
                }
                Last::Ack => {
                    self.out_pos = self.out_len;
                    return self.emit(id, resp);
                }
                _ => {}
            }
        }
        let Some((&flags, mut payload)) = data.split_first() else { return self.fail() };
        if flags & FLAG_LENGTH != 0 {
            if payload.len() < 4 {
                return self.fail();
            }
            payload = &payload[4..];
        }
        if flags & FLAG_START != 0 {
            if self.started {
                return self.fail();
            }
            self.started = true;
            // Version bits: we answer 0 (PEAPv0, and the only EAP-TLS there is).
            if !self.collect(tls) {
                return self.fail();
            }
            return self.emit(id, resp);
        }
        if !self.started {
            return self.fail();
        }
        if self.out_pos < self.out_len {
            // In the middle of our own message: only an acknowledgement fits.
            if !payload.is_empty() || flags & FLAG_MORE != 0 {
                return self.fail();
            }
            return self.emit(id, resp);
        }
        self.out_len = 0;
        self.out_pos = 0;
        self.out_from = 0;
        if !payload.is_empty() && !tls.input(payload) {
            return self.fail();
        }
        if flags & FLAG_MORE != 0 {
            return self.emit(id, resp);
        }
        if tls.established() {
            if self.key == [0; 64] {
                let mut material = [0u8; 64];
                let ok = tls.export(KEY_LABEL, &mut material);
                self.key = material;
                wipe(&mut material);
                if !ok || self.key == [0; 64] {
                    return self.fail();
                }
            }
            match self.method {
                Method::Tls => self.done = true,
                Method::Peap => {
                    let mut plain = [0u8; 640];
                    loop {
                        let n = tls.read(&mut plain);
                        if n == 0 {
                            break;
                        }
                        if n == plain.len() {
                            wipe(&mut plain);
                            return self.fail();
                        }
                        // One read can hold several inner messages: answer each.
                        let mut at = 0;
                        while at < n {
                            let len = inner_len(&plain[at..n]);
                            if len == 0 || !self.phase2(tls, &plain[at..at + len]) {
                                wipe(&mut plain);
                                return self.fail();
                            }
                            at += len;
                        }
                    }
                    wipe(&mut plain);
                }
            }
        }
        if !self.collect(tls) {
            return self.fail();
        }
        self.emit(id, resp)
    }

    /// One decrypted PEAPv0 message (`inner_len` cut it out). Inside the tunnel EAP packets travel
    /// without their four-byte header, except the TLV exchange at the end.
    fn phase2(&mut self, tls: &mut dyn Tls, p: &[u8]) -> bool {
        if p.len() >= 5 && p[0] == REQUEST && p[4] == TYPE_TLV {
            return self.result(tls, p);
        }
        let body = if p.len() == 5 && p[0] == REQUEST && p[2..4] == [0, 5] && p[4] == TYPE_IDENTITY { &p[4..] } else { p };
        let Some((&kind, data)) = body.split_first() else { return false };
        let mut answer = [0u8; 160];
        let n = match kind {
            TYPE_IDENTITY => {
                let user = self.user;
                answer[0] = TYPE_IDENTITY;
                answer[1..1 + user.get().len()].copy_from_slice(user.get());
                1 + user.get().len()
            }
            TYPE_MSCHAPV2 => match self.mschapv2(data, &mut answer) {
                Some(n) => n,
                None => return false,
            },
            _ => {
                answer[..2].copy_from_slice(&[TYPE_NAK, TYPE_MSCHAPV2]);
                2
            }
        };
        let ok = tls.write(&answer[..n]);
        wipe(&mut answer);
        ok
    }

    fn mschapv2(&mut self, d: &[u8], answer: &mut [u8; 160]) -> Option<usize> {
        // op, identifier, length (2), then the operation's own fields.
        if d.len() < 4 {
            return None;
        }
        let stated = usize::from(u16::from_be_bytes([d[2], d[3]]));
        if stated < 4 || stated > d.len() {
            return None;
        }
        let (op, ms_id, rest) = (d[0], d[1], &d[4..stated]);
        answer[0] = TYPE_MSCHAPV2;
        match op {
            // Challenge: size (16), challenge, server name.
            1 if self.inner == Inner::Idle => {
                if rest.len() < 17 || rest[0] != 16 {
                    return None;
                }
                let mut server = [0u8; 16];
                server.copy_from_slice(&rest[1..17]);
                let user = self.user;
                let mut hash = mschapv2::nt_hash(self.password.get())?;
                let nt = mschapv2::nt_response(&server, &self.peer_challenge, user.get(), &hash);
                self.proof = mschapv2::authenticator_response(&hash, &nt, &self.peer_challenge, &server, user.get());
                self.master = mschapv2::master_key(&hash, &nt);
                wipe(&mut hash);
                self.inner = Inner::Answered;
                let name = user.get();
                let ms_len = 4 + 1 + 49 + name.len();
                answer[1] = 2;
                answer[2] = ms_id;
                answer[3..5].copy_from_slice(&(ms_len as u16).to_be_bytes());
                answer[5] = 49;
                answer[6..22].copy_from_slice(&self.peer_challenge);
                answer[22..30].fill(0);
                answer[30..54].copy_from_slice(&nt);
                answer[54] = 0;
                answer[55..55 + name.len()].copy_from_slice(name);
                Some(55 + name.len())
            }
            // Success: "S=" and 40 hex digits of the authenticator response.
            3 if self.inner == Inner::Answered => {
                if rest.len() < 42 || &rest[..2] != b"S=" {
                    return None;
                }
                let mut got = [0u8; 20];
                for (n, byte) in got.iter_mut().enumerate() {
                    let hi = (rest[2 + 2 * n] as char).to_digit(16)?;
                    let lo = (rest[3 + 2 * n] as char).to_digit(16)?;
                    *byte = (hi * 16 + lo) as u8;
                }
                if !same(&got, &self.proof) {
                    // The server does not know the password: not our network.
                    return None;
                }
                self.inner = Inner::Proven;
                answer[1] = 3;
                Some(2)
            }
            // Failure: the server refused the account. Acknowledge; it ends there.
            4 if self.inner == Inner::Answered => {
                self.inner = Inner::Rejected;
                answer[1] = 4;
                Some(2)
            }
            _ => None,
        }
    }

    /// The protected result (EAP-TLV): Result TLV and, if the server sent
    /// one, the cryptobinding TLV that ties the tunnel to the inner keys.
    fn result(&mut self, tls: &mut dyn Tls, p: &[u8]) -> bool {
        let len = usize::from(u16::from_be_bytes([p[2], p[3]]));
        if len < 5 || len > p.len() || self.done {
            return false;
        }
        let mut tlvs = &p[5..len];
        let mut status = 0u16;
        let mut binding: Option<&[u8]> = None;
        while !tlvs.is_empty() {
            if tlvs.len() < 4 {
                return false;
            }
            let kind = u16::from_be_bytes([tlvs[0], tlvs[1]]);
            let n = usize::from(u16::from_be_bytes([tlvs[2], tlvs[3]]));
            if tlvs.len() < 4 + n {
                return false;
            }
            match (kind & 0x3fff, n) {
                (3, 2) => status = u16::from_be_bytes([tlvs[4], tlvs[5]]),
                (12, 56) => binding = Some(&tlvs[..60]),
                // Something mandatory we do not understand.
                _ if kind & 0x8000 != 0 => return false,
                _ => {}
            }
            tlvs = &tlvs[4 + n..];
        }
        let mut ok = status == 1 && self.inner == Inner::Proven && (binding.is_some() || !self.require_binding);
        let mut cmk = [0u8; 20];
        let mut nonce = [0u8; 32];
        if let (true, Some(b)) = (ok, binding) {
            // ISK: the inner method's keys, client send key first.
            let mut isk = [0u8; 32];
            isk[..16].copy_from_slice(&mschapv2::start_key(&self.master, true));
            isk[16..].copy_from_slice(&mschapv2::start_key(&self.master, false));
            let mut imck = [0u8; 60];
            prf_plus(&self.key[..40], b"Inner Methods Compound Keys", &isk, &mut imck);
            self.ipmk.copy_from_slice(&imck[..40]);
            cmk.copy_from_slice(&imck[40..]);
            wipe(&mut isk);
            wipe(&mut imck);
            // reserved, version, received version, subtype (0: request), nonce, MAC.
            let mut signed = [0u8; 61];
            signed[..40].copy_from_slice(&b[..40]);
            signed[60] = TYPE_PEAP;
            ok = b[5] == 0 && b[7] == 0 && same(&hmac_sha1(&cmk, &[&signed]), &b[40..60]);
            nonce.copy_from_slice(&b[8..40]);
        }
        let mut answer = [0u8; 5 + 6 + 60];
        let mut n = 11;
        answer[0] = RESPONSE;
        answer[1] = p[1];
        answer[4] = TYPE_TLV;
        answer[5..11].copy_from_slice(&[0x80, 3, 0, 2, 0, if ok { 1 } else { 2 }]);
        if ok && binding.is_some() {
            answer[11..19].copy_from_slice(&[0, 12, 0, 56, 0, 0, 0, 1]);
            answer[19..51].copy_from_slice(&nonce);
            let mut signed = [0u8; 61];
            signed[..40].copy_from_slice(&answer[11..51]);
            signed[60] = TYPE_PEAP;
            let mac = hmac_sha1(&cmk, &[&signed]);
            answer[51..71].copy_from_slice(&mac);
            n = 71;
        }
        wipe(&mut cmk);
        answer[2..4].copy_from_slice(&(n as u16).to_be_bytes());
        if !tls.write(&answer[..n]) {
            return false;
        }
        if !ok {
            // The refusal goes out; no Success will be believed after it.
            self.inner = Inner::Rejected;
            self.done = false;
            return true;
        }
        if binding.is_some() {
            self.binding_used = true;
            // The session key comes from the compound key, not the tunnel alone.
            let mut csk = [0u8; 64];
            let ipmk = self.ipmk;
            prf_plus(&ipmk, b"Session Key Generating Function", &[0], &mut csk);
            self.key = csk;
            wipe(&mut csk);
        }
        self.done = true;
        true
    }
}
