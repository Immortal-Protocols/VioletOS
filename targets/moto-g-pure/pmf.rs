//! IEEE 802.11w Protected Management Frames for one station link, in software.
//!
//! Once a link has negotiated management frame protection, a deauthentication
//! or disassociation ends it only when it is authentic: a broadcast one must
//! carry a valid BIP-CMAC-128 MIC under the IGTK with a fresh packet number, a
//! unicast one must decrypt under the pairwise key with CCMP-128 and a fresh
//! packet number. An unprotected one is dropped, unless its reason says the
//! access point no longer knows us (6 or 7): that starts the SA Query procedure,
//! a protected question the access point has one second to answer. SA Query
//! requests from the access point are answered, so it can tell a forged
//! association from us.
//!
//! What an unanswered query means depends on the build. The exchange has not
//! been seen working on the handset's firmware, so by default silence alone
//! does not end the link (a forged frame would then still end it, a second
//! late): the link is only in doubt, and ends when eight frames sent over five
//! seconds or more bring no unicast frame back under the pairwise key. Built
//! with VIOLET_WIFI_SA_QUERY_TIMEOUT set, an unanswered query ends the link
//! at once, as the standard has it.
//!
//! No hardware access and no allocation: Radio calls this with received frames
//! and sends what it returns. AES is hostap's on the handset.

/// Longest frame `Link` asks Radio to send (an SA Query: 24 + 8 + 4 + 8).
pub const REPLY: usize = 48;
/// An unanswered SA Query ends the link by itself.
const TIMEOUT_ENDS_LINK: bool = option_env!("VIOLET_WIFI_SA_QUERY_TIMEOUT").is_some();
/// Longer than this between two looks at the receive rings and the answer to a
/// pending query may have been missed: the query starts again.
const QUERY_GAP: u32 = 100;
/// A link in doubt ends after this many frames sent over this long, unanswered.
const DOUBT_FRAMES: u8 = 8;
const DOUBT_LIMIT: u32 = 5000;
/// Longest protected management body that is decrypted. Longer ones are not
/// frames this station acts on.
const PLAIN: usize = 128;
/// dot11AssociationSAQueryRetryTimeout and ...MaximumTimeout, in milliseconds.
const QUERY_RETRY: u32 = 200;
const QUERY_LIMIT: u32 = 1000;

// `violet_hostap_aes` lets a host test run on the handset's cipher (make test-wpa).
#[cfg(any(not(test), violet_hostap_aes))]
mod cipher {
    use core::ffi::c_void;
    extern "C" {
        fn aes_encrypt_init(key: *const u8, len: usize) -> *mut c_void;
        fn aes_encrypt(ctx: *mut c_void, plain: *const u8, crypt: *mut u8) -> i32;
        fn aes_encrypt_deinit(ctx: *mut c_void);
        fn omac1_aes_128_vector(key: *const u8, parts: usize, part: *const *const u8, len: *const usize, mac: *mut u8) -> i32;
    }
    /// hostap's AES-CMAC (RFC 4493) of the concatenation of up to three parts.
    pub fn cmac(key: &[u8; 16], parts: &[&[u8]]) -> Option<[u8; 16]> {
        let (mut part, mut len, mut mac) = ([core::ptr::null(); 3], [0; 3], [0; 16]);
        let mut n = 0;
        // hostap reads the first byte of every part it is given: none may be empty.
        for p in parts.iter().filter(|p| !p.is_empty()) {
            (part[n], len[n]) = (p.as_ptr(), p.len());
            n += 1;
        }
        let rc = unsafe { omac1_aes_128_vector(key.as_ptr(), n, part.as_ptr(), len.as_ptr(), mac.as_mut_ptr()) };
        (rc == 0).then_some(mac)
    }
    /// hostap's AES-128 key schedule (freed, and wiped by Radio's allocator, on drop).
    pub struct Aes(*mut c_void);
    impl Aes {
        pub fn new(key: &[u8; 16]) -> Option<Self> {
            let ctx = unsafe { aes_encrypt_init(key.as_ptr(), 16) };
            (!ctx.is_null()).then_some(Self(ctx))
        }
        pub fn block(&self, b: &mut [u8; 16]) {
            unsafe { aes_encrypt(self.0, b.as_ptr(), b.as_mut_ptr()) };
        }
    }
    impl Drop for Aes {
        fn drop(&mut self) {
            unsafe { aes_encrypt_deinit(self.0) }
        }
    }
}

/// Host tests carry their own AES-128 (checked against FIPS-197 below).
#[cfg(all(test, not(violet_hostap_aes)))]
mod cipher {
    pub struct Aes([[u8; 16]; 11], [u8; 256]);
    thread_local! {
        /// Set: no key schedule can be made, as when hostap's arena is full.
        pub static EXHAUSTED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    }
    fn sbox() -> [u8; 256] {
        let mut s = [0u8; 256];
        let (mut p, mut q) = (1u8, 1u8);
        loop {
            p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
            q ^= q << 1;
            q ^= q << 2;
            q ^= q << 4;
            if q & 0x80 != 0 {
                q ^= 0x09;
            }
            s[p as usize] = 0x63 ^ q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
            if p == 1 {
                break;
            }
        }
        s[0] = 0x63;
        s
    }
    fn double(b: u8) -> u8 {
        (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
    }
    impl Aes {
        pub fn new(key: &[u8; 16]) -> Option<Self> {
            if EXHAUSTED.with(|e| e.get()) {
                return None;
            }
            let s = sbox();
            let mut keys = [[0u8; 16]; 11];
            keys[0] = *key;
            let mut rcon = 1u8;
            for r in 1..11 {
                let prev = keys[r - 1];
                let mut word = [s[prev[13] as usize] ^ rcon, s[prev[14] as usize], s[prev[15] as usize], s[prev[12] as usize]];
                rcon = double(rcon);
                for i in 0..16 {
                    word[i % 4] ^= prev[i];
                    keys[r][i] = word[i % 4];
                }
            }
            Some(Self(keys, s))
        }
        pub fn block(&self, b: &mut [u8; 16]) {
            let xor = |b: &mut [u8; 16], k: &[u8; 16]| b.iter_mut().zip(k).for_each(|(b, k)| *b ^= k);
            xor(b, &self.0[0]);
            for round in 1..11 {
                let mut t = [0u8; 16];
                for i in 0..16 {
                    // SubBytes and ShiftRows: row r of column c comes from column c + r.
                    t[i] = self.1[b[(i + 4 * (i % 4)) % 16] as usize];
                }
                if round != 10 {
                    for c in t.chunks_exact_mut(4) {
                        let all = c[0] ^ c[1] ^ c[2] ^ c[3];
                        let first = c[0];
                        for i in 0..4 {
                            let next = if i == 3 { first } else { c[i + 1] };
                            c[i] ^= all ^ double(c[i] ^ next);
                        }
                    }
                }
                *b = t;
                xor(b, &self.0[round]);
            }
        }
    }
    /// AES-CMAC (RFC 4493) of the concatenation of `parts`: the host tests' stand-in for hostap's.
    pub fn cmac(key: &[u8; 16], parts: &[&[u8]]) -> Option<[u8; 16]> {
        let aes = &Aes::new(key)?;
        fn double(b: &mut [u8; 16]) {
            let carry = b[0] >> 7;
            for i in 0..15 {
                b[i] = b[i] << 1 | b[i + 1] >> 7;
            }
            b[15] = b[15] << 1 ^ carry * 0x87;
        }
        let mut k = [0u8; 16];
        aes.block(&mut k);
        double(&mut k); // K1
        let mut x = [0u8; 16];
        let mut n = 0;
        // A full block is encrypted only once another byte follows it, so the
        // last block (full or not) is still open at the end.
        for part in parts {
            for &byte in part.iter() {
                if n == 16 {
                    aes.block(&mut x);
                    n = 0;
                }
                x[n] ^= byte;
                n += 1;
            }
        }
        if n != 16 {
            x[n] ^= 0x80;
            double(&mut k); // K2
        }
        for i in 0..16 {
            x[i] ^= k[i];
        }
        aes.block(&mut x);
        Some(x)
    }
}
use cipher::{cmac, Aes};

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// AES-CCM with an 8-byte tag and a 2-byte length (RFC 3610; CCMP-128's
/// parameters). Transforms `data` in place and returns the tag of the plaintext.
/// `aad` is 1 to 30 bytes.
#[inline(never)]
fn ccm(aes: &Aes, nonce: &[u8; 13], aad: &[u8], data: &mut [u8], decrypt: bool) -> [u8; 8] {
    let mut x = [0u8; 16];
    x[0] = 0x59;
    x[1..14].copy_from_slice(nonce);
    x[14] = (data.len() >> 8) as u8;
    x[15] = data.len() as u8;
    aes.block(&mut x);
    x[1] ^= aad.len() as u8;
    let mut n = 2;
    for &byte in aad {
        if n == 16 {
            aes.block(&mut x);
            n = 0;
        }
        x[n] ^= byte;
        n += 1;
    }
    aes.block(&mut x);
    let mut counter = [0u8; 16];
    counter[0] = 1;
    counter[1..14].copy_from_slice(nonce);
    for (i, chunk) in data.chunks_mut(16).enumerate() {
        let mut s = counter;
        s[14] = ((i + 1) >> 8) as u8;
        s[15] = (i + 1) as u8;
        aes.block(&mut s);
        for (k, b) in chunk.iter_mut().enumerate() {
            if decrypt {
                *b ^= s[k];
                x[k] ^= *b;
            } else {
                x[k] ^= *b;
                *b ^= s[k];
            }
        }
        aes.block(&mut x);
    }
    aes.block(&mut counter);
    let mut tag = [0u8; 8];
    for i in 0..8 {
        tag[i] = x[i] ^ counter[i];
    }
    tag
}

/// CCMP additional authenticated data and nonce for an MPDU with a 24-byte
/// header (IEEE 802.11-2020 12.5.3.3.3 and 12.5.3.3.4): Retry, Power Management
/// and More Data are masked, Protected is set, the sequence number is masked.
/// Management frames set the Management bit of the nonce flags. (For a data
/// frame this is right only without QoS and with subtype 0: the test vector.)
#[inline(never)]
fn ccmp_inputs(header: &[u8], pn: u64, management: bool) -> ([u8; 22], [u8; 13]) {
    let mut aad = [0u8; 22];
    aad[0] = header[0];
    aad[1] = (header[1] & !0x38) | 0x40;
    aad[2..20].copy_from_slice(&header[4..22]);
    aad[20] = header[22] & 0x0f;
    let mut nonce = [0u8; 13];
    nonce[0] = if management { 0x10 } else { 0 };
    nonce[1..7].copy_from_slice(&header[10..16]);
    nonce[7..13].copy_from_slice(&pn.to_be_bytes()[2..]);
    (aad, nonce)
}

/// What Radio does with a management frame, or on a timer pass.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Verdict {
    /// Nothing: not for this link, not authentic, or handled.
    Ignore,
    /// The link is over: associate again.
    Disconnect,
    /// Send the first `n` bytes of the reply buffer as an 802.11 frame, as they are.
    Send(usize),
}

pub struct Link {
    /// Management frame protection was negotiated for this association.
    active: bool,
    /// The pairwise temporal key is installed.
    keyed: bool,
    tk: [u8; 16],
    /// IGTKs with key IDs 4 and 5, and the last packet number accepted under each.
    igtk: [[u8; 16]; 2],
    igtk_set: [bool; 2],
    ipn: [u64; 2],
    /// Last packet number accepted in a unicast management frame, and last sent.
    rx_pn: u64,
    tx_pn: u64,
    /// An SA Query is waiting for its answer.
    querying: bool,
    query_id: u16,
    query_start: u32,
    query_sent: u32,
    /// When `tick` last ran.
    query_seen: u32,
    /// An unanswered SA Query ends the link (else it only puts it in doubt).
    timeout_ends: bool,
    /// A query went unanswered: frames sent since, and since when.
    doubt: bool,
    doubt_sent: u8,
    doubt_start: u32,
}

impl Link {
    pub const fn new() -> Self {
        Self {
            active: false,
            keyed: false,
            tk: [0; 16],
            igtk: [[0; 16]; 2],
            igtk_set: [false; 2],
            ipn: [0; 2],
            rx_pn: 0,
            tx_pn: 0,
            querying: false,
            query_id: 0,
            query_start: 0,
            query_sent: 0,
            query_seen: 0,
            timeout_ends: TIMEOUT_ENDS_LINK,
            doubt: false,
            doubt_sent: 0,
            doubt_start: 0,
        }
    }
    /// A link ends or a new association starts: wipe every key. `active` says
    /// whether the association now starting negotiated management frame protection.
    #[inline(never)]
    pub fn reset(&mut self, active: bool) {
        for byte in self.tk.iter_mut().chain(self.igtk.iter_mut().flatten()) {
            // Not optimised away, although the keys are not read again.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        let (id, ends) = (self.query_id, self.timeout_ends);
        *self = Self::new();
        self.active = active;
        self.query_id = id;
        self.timeout_ends = ends;
    }
    pub fn active(&self) -> bool {
        self.active
    }
    /// Host tests choose what an unanswered SA Query means; builds do with
    /// VIOLET_WIFI_SA_QUERY_TIMEOUT.
    #[cfg(test)]
    pub fn timeout_ends(&mut self, ends: bool) {
        self.timeout_ends = ends;
    }
    /// An SA Query is pending or the link is in doubt: call `tick`.
    pub fn pending(&self) -> bool {
        self.querying || self.doubt
    }
    /// A unicast data frame from the access point decrypted under the pairwise
    /// key: it still holds this association, so the link is not in doubt.
    pub fn heard(&mut self) {
        self.doubt = false;
    }
    /// A data frame was handed to the firmware for the access point.
    pub fn sent(&mut self) {
        self.doubt_sent = self.doubt_sent.saturating_add(1);
    }

    /// The pairwise key of this association (again after a rekey: packet numbers restart).
    pub fn set_tk(&mut self, key: &[u8]) {
        if self.active && key.len() == 16 {
            self.tk.copy_from_slice(key);
            self.keyed = true;
            self.rx_pn = 0;
            self.tx_pn = 0;
        }
    }
    /// Install the IGTK with key ID 4 or 5 and its current packet number
    /// (little-endian, as in the key data). False: not a key this link can use.
    pub fn set_igtk(&mut self, id: u8, key: &[u8], ipn: &[u8]) -> bool {
        if !self.active || !matches!(id, 4 | 5) || key.len() != 16 || ipn.len() != 6 {
            return false;
        }
        let slot = usize::from(id - 4);
        self.igtk[slot].copy_from_slice(key);
        self.ipn[slot] = little_48(ipn);
        self.igtk_set[slot] = true;
        true
    }

    /// Judge a management frame received while joined. `station` is our address,
    /// `ap` the access point's; `now` is Radio's tick counter.
    #[inline(never)]
    pub fn receive(
        &mut self,
        frame: &[u8],
        station: &[u8; 6],
        ap: &[u8; 6],
        now: u32,
        reply: &mut [u8; REPLY],
    ) -> Verdict {
        if frame.len() < 26 || frame[0] & 0x0c != 0 || !at(frame, 10, ap) || !at(frame, 16, ap) {
            return Verdict::Ignore;
        }
        let to_us = at(frame, 4, station);
        let broadcast = at(frame, 4, &[0xff; 6]);
        let leaving = matches!(frame[0], 0xa0 | 0xc0);
        if !self.active || !self.keyed {
            // No protection on this link, or its keys are not installed yet:
            // the access point's word ends it, as before 802.11w.
            return if leaving && (to_us || broadcast) { Verdict::Disconnect } else { Verdict::Ignore };
        }
        // An unprotected leave is dropped. Only "you are not authenticated" (6)
        // and "not associated" (7), which an access point that lost our state
        // sends in the clear, are worth asking it about (IEEE 802.11-2020 11.13).
        let asks = leaving && frame[1] & 0x40 == 0 && matches!(frame[24], 6 | 7) && frame[25] == 0;
        if leaving && broadcast {
            return if self.bip(frame) {
                Verdict::Disconnect
            } else if asks {
                self.suspect(station, ap, now, reply)
            } else {
                Verdict::Ignore
            };
        }
        if !to_us {
            return Verdict::Ignore;
        }
        let mut plain = [0u8; PLAIN];
        let Some(n) = self.open(frame, &mut plain) else {
            return if asks { self.suspect(station, ap, now, reply) } else { Verdict::Ignore };
        };
        if leaving {
            return if n >= 2 { Verdict::Disconnect } else { Verdict::Ignore };
        }
        // SA Query (category 8): answer a request, accept the answer to ours.
        if frame[0] == 0xd0 && n >= 4 && plain[0] == 8 {
            if plain[1] == 0 {
                return self.seal(station, ap, [1, plain[2], plain[3]], reply);
            }
            if plain[1] == 1 && self.querying && plain[2..4] == self.query_id.to_le_bytes() {
                self.querying = false;
            }
        }
        Verdict::Ignore
    }

    /// Call on every look at the receive rings while `pending`, after the
    /// frames found there went through `receive`. `idle`: the rings were empty,
    /// so no answer is waiting unread. Repeats a pending SA Query; decides what
    /// the access point's silence means (see the top of this file).
    #[inline(never)]
    pub fn tick(
        &mut self,
        station: &[u8; 6],
        ap: &[u8; 6],
        now: u32,
        expired: fn(u32, u32) -> bool,
        idle: bool,
        reply: &mut [u8; REPLY],
    ) -> Verdict {
        if idle && self.doubt && self.doubt_sent >= DOUBT_FRAMES && expired(self.doubt_start, DOUBT_LIMIT) {
            self.doubt = false;
            return Verdict::Disconnect;
        }
        if !self.querying {
            return Verdict::Ignore;
        }
        let paused = expired(self.query_seen, QUERY_GAP);
        self.query_seen = now;
        if paused {
            // Radio was busy elsewhere (a blocking send, a rekey, a request):
            // the answer may have come and gone unread. Ask again, from the start.
            self.query_start = now;
        } else if idle && expired(self.query_start, QUERY_LIMIT) {
            self.querying = false;
            if self.timeout_ends {
                return Verdict::Disconnect;
            }
            if !self.doubt {
                (self.doubt, self.doubt_sent, self.doubt_start) = (true, 0, now);
            }
            return Verdict::Ignore;
        }
        if paused || expired(self.query_sent, QUERY_RETRY) {
            self.query_sent = now;
            let id = self.query_id.to_le_bytes();
            return self.seal(station, ap, [0, id[0], id[1]], reply);
        }
        Verdict::Ignore
    }

    /// A deauthentication or disassociation that says the access point does
    /// not know us, unauthenticated: ask the access point.
    #[inline(never)]
    fn suspect(&mut self, station: &[u8; 6], ap: &[u8; 6], now: u32, reply: &mut [u8; REPLY]) -> Verdict {
        if self.querying {
            return Verdict::Ignore;
        }
        let id = self.query_id.wrapping_add(1).to_le_bytes();
        let verdict = self.seal(station, ap, [0, id[0], id[1]], reply);
        // No query without a frame to send: nothing would ever answer it.
        if verdict != Verdict::Ignore {
            self.querying = true;
            self.query_id = u16::from_le_bytes(id);
            (self.query_start, self.query_sent, self.query_seen) = (now, now, now);
        }
        verdict
    }

    /// Build a CCMP-protected SA Query frame to the access point; `body` is
    /// the action (0 request, 1 response) and the transaction.
    #[inline(never)]
    fn seal(&mut self, station: &[u8; 6], ap: &[u8; 6], body: [u8; 3], out: &mut [u8; REPLY]) -> Verdict {
        let Some(aes) = Aes::new(&self.tk) else {
            return Verdict::Ignore;
        };
        self.tx_pn += 1;
        let pn = self.tx_pn.to_le_bytes();
        out.fill(0);
        out[0] = 0xd0;
        out[1] = 0x40;
        out[4..10].copy_from_slice(ap);
        out[10..16].copy_from_slice(station);
        out[16..22].copy_from_slice(ap);
        out[24..32].copy_from_slice(&[pn[0], pn[1], 0, 0x20, pn[2], pn[3], pn[4], pn[5]]);
        out[32] = 8;
        out[33..36].copy_from_slice(&body);
        let (aad, nonce) = ccmp_inputs(&out[..24], self.tx_pn, true);
        let tag = ccm(&aes, &nonce, &aad, &mut out[32..36], false);
        out[36..44].copy_from_slice(&tag);
        Verdict::Send(44)
    }

    /// Decrypt a CCMP-protected unicast management frame into `plain`. None
    /// unless its MIC is right and its packet number is new.
    #[inline(never)]
    fn open(&mut self, frame: &[u8], plain: &mut [u8; PLAIN]) -> Option<usize> {
        // Protected, no HT Control field, CCMP header with key ID 0.
        if frame[1] & 0xc0 != 0x40 {
            return None;
        }
        let header = frame.get(24..32)?;
        let body = frame.get(32..)?;
        let n = body.len().checked_sub(8)?;
        if n > PLAIN || header[3] & 0xe0 != 0x20 {
            return None;
        }
        let pn = little_48(&[header[0], header[1], header[4], header[5], header[6], header[7]]);
        if pn <= self.rx_pn {
            return None;
        }
        let aes = Aes::new(&self.tk)?;
        plain[..n].copy_from_slice(&body[..n]);
        let (aad, nonce) = ccmp_inputs(frame, pn, true);
        let tag = ccm(&aes, &nonce, &aad, &mut plain[..n], true);
        if !same(&tag, &body[n..]) {
            plain.fill(0);
            return None;
        }
        self.rx_pn = pn;
        Some(n)
    }

    /// Check the BIP-CMAC-128 MIC of a group-addressed management frame (IEEE
    /// 802.11-2020 12.5.4): its last element is the MME (ID 76: key ID, packet
    /// number, MIC), and the MIC covers the masked frame control, the three
    /// addresses and the body with the MIC field zero.
    #[inline(never)]
    fn bip(&mut self, frame: &[u8]) -> bool {
        let Some(at) = frame.len().checked_sub(18).filter(|&at| at >= 26) else {
            return false;
        };
        let mme = &frame[at..];
        if mme[..2] != [76, 16] || mme[3] != 0 || !matches!(mme[2], 4 | 5) {
            return false;
        }
        let slot = usize::from(mme[2] - 4);
        let ipn = little_48(&mme[4..10]);
        if !self.igtk_set[slot] || ipn <= self.ipn[slot] {
            return false;
        }
        let mut aad = [0u8; 20];
        aad[0] = frame[0];
        aad[1] = frame[1] & !0x38;
        aad[2..].copy_from_slice(&frame[4..22]);
        let Some(mic) = cmac(&self.igtk[slot], &[&aad, &frame[24..at + 10], &[0; 8]]) else {
            return false;
        };
        if !same(&mic[..8], &mme[10..]) {
            return false;
        }
        self.ipn[slot] = ipn;
        true
    }
}

/// The address at `offset` of a frame of at least 24 bytes is `address`.
#[inline(never)]
fn at(frame: &[u8], offset: usize, address: &[u8; 6]) -> bool {
    frame[offset..offset + 6] == *address
}

fn little_48(b: &[u8]) -> u64 {
    b[..6].iter().rev().fold(0, |v, &b| v << 8 | u64::from(b))
}

/// The access point's side, for host tests here and in test_radio.rs.
#[cfg(test)]
pub mod access_point {
    use super::*;

    /// A management frame from the access point, as Radio receives it.
    pub fn frame(kind: u8, ap: &[u8; 6], to: &[u8; 6], body: &[u8]) -> Vec<u8> {
        let mut f = vec![kind, 0, 0, 0];
        f.extend_from_slice(to);
        f.extend_from_slice(ap);
        f.extend_from_slice(ap);
        f.extend_from_slice(&[0x30, 0x12]);
        f.extend_from_slice(body);
        f
    }
    /// The access point's side of BIP: append the MME.
    pub fn bip_protect(mut f: Vec<u8>, igtk: &[u8; 16], id: u8, ipn: u64) -> Vec<u8> {
        f.extend_from_slice(&[76, 16, id, 0]);
        f.extend_from_slice(&ipn.to_le_bytes()[..6]);
        let mut aad = vec![f[0], f[1] & !0x38];
        aad.extend_from_slice(&f[4..22]);
        let mic = cmac(igtk, &[&aad, &f[24..], &[0; 8]]).unwrap();
        f.extend_from_slice(&mic[..8]);
        f
    }
    /// The access point's side of CCMP for a unicast management frame.
    pub fn ccmp_protect(f: &[u8], tk: &[u8; 16], pn: u64) -> Vec<u8> {
        let p = pn.to_le_bytes();
        let mut out = f[..24].to_vec();
        out[1] |= 0x40;
        out.extend_from_slice(&[p[0], p[1], 0, 0x20, p[2], p[3], p[4], p[5]]);
        let mut body = f[24..].to_vec();
        let (aad, nonce) = ccmp_inputs(&out, pn, true);
        let tag = ccm(&Aes::new(tk).unwrap(), &nonce, &aad, &mut body, false);
        out.extend_from_slice(&body);
        out.extend_from_slice(&tag);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        s.chunks(2).map(|p| u8::from_str_radix(core::str::from_utf8(p).unwrap(), 16).unwrap()).collect()
    }
    fn key(s: &str) -> [u8; 16] {
        hex(s).try_into().unwrap()
    }

    #[test]
    fn test_aes_matches_fips_197() {
        // FIPS-197 appendix C.1.
        let aes = Aes::new(&key("000102030405060708090a0b0c0d0e0f")).unwrap();
        let mut block = key("00112233445566778899aabbccddeeff");
        aes.block(&mut block);
        assert_eq!(block.to_vec(), hex("69c4e0d86a7b0430d8cdb78070b4c55a"));
    }

    #[test]
    fn cmac_matches_rfc_4493() {
        let aes = key("2b7e151628aed2a6abf7158809cf4f3c");
        let m = hex("6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51
                     30c81c46a35ce411e5fbc1191a0a52ef f69f2445df4f9b17ad2b417be66c3710");
        for (len, mac) in [
            (0, "bb1d6929e95937287fa37d129b756746"),
            (16, "070a16b46b4d4144f79bdd9dd04a287c"),
            (40, "dfa66747de9ae63030ca32611497c827"),
            (64, "51f0bebf7e3b9d92fc49741779363cfe"),
        ] {
            assert_eq!(cmac(&aes, &[&m[..len]]).unwrap().to_vec(), hex(mac), "{len} bytes");
            // The same message in pieces, split inside and at block boundaries.
            for cut in [0, 1, 16, 17, 32] {
                let cut = cut.min(len);
                assert_eq!(cmac(&aes, &[&m[..cut], &[], &m[cut..len]]).unwrap().to_vec(), hex(mac));
            }
        }
    }

    #[test]
    fn ccm_matches_rfc_3610_packet_vector_1() {
        let aes = Aes::new(&key("c0c1c2c3c4c5c6c7c8c9cacbcccdcecf")).unwrap();
        let nonce: [u8; 13] = hex("00000003020100a0a1a2a3a4a5").try_into().unwrap();
        let packet: Vec<u8> = (0..31).collect();
        let mut data = packet[8..].to_vec();
        let tag = ccm(&aes, &nonce, &packet[..8], &mut data, false);
        assert_eq!(data, hex("588c979a61c663d2f066d0c2c0f989806d5f6b61dac384"));
        assert_eq!(tag.to_vec(), hex("17e8d12cfdf926e0"));
        assert_eq!(ccm(&aes, &nonce, &packet[..8], &mut data, true), tag);
        assert_eq!(data, packet[8..]);
    }

    #[test]
    fn ccmp_matches_the_ieee_802_11_test_vector() {
        // The CCMP-128 data MPDU example of IEEE 802.11's annex of test vectors
        // (from 802.11i-2004 Annex H), with its published ciphertext and MIC.
        let aes = Aes::new(&key("c97c1f67ce371185514a8a19f2bdd52f")).unwrap();
        let header = hex("0848c32c 0fd2e128a57c 5030f1844408 abaea5b8fcba 8033");
        let mut data = hex("f8ba1a55d02f85ae967bb62fb6cda8eb7e78a050");
        let (aad, nonce) = ccmp_inputs(&header, 0xb5039776e70c, false);
        assert_eq!(aad.to_vec(), hex("0840 0fd2e128a57c 5030f1844408 abaea5b8fcba 0000"));
        assert_eq!(nonce.to_vec(), hex("00 5030f1844408 b5039776e70c"));
        let tag = ccm(&aes, &nonce, &aad, &mut data, false);
        assert_eq!(data, hex("f3d0a2fe9a3dbf2342a643e43246e80c3c04d019"));
        assert_eq!(tag.to_vec(), hex("7845ce0b16f97623"));
    }

    const STA: [u8; 6] = [2, 0, 0, 0, 1, 0];
    const AP: [u8; 6] = [2, 0, 0, 0, 0, 0];
    const TK: &str = "66ed21042f9f26d7115706e40414cf2e";
    const IGTK: &str = "4ea9543e09cf2b1eca66ffc58bdecbcf";

    use super::access_point::{bip_protect, ccmp_protect, frame};
    fn link() -> Link {
        let mut link = Link::new();
        link.reset(true);
        link.set_tk(&key(TK));
        assert!(link.set_igtk(4, &key(IGTK), &[3, 0, 0, 0, 0, 0]));
        link
    }
    /// A link whose unanswered SA Query ends it (VIOLET_WIFI_SA_QUERY_TIMEOUT).
    fn strict() -> Link {
        let mut link = link();
        link.timeout_ends = true;
        link
    }
    thread_local! {
        /// Radio's clock, in milliseconds.
        static CLOCK: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
    }
    fn expired(start: u32, ms: u32) -> bool {
        CLOCK.with(|c| c.get()).wrapping_sub(start) >= ms
    }
    /// One look at empty receive rings at time `at`.
    fn tick(link: &mut Link, at: u32, reply: &mut [u8; REPLY]) -> Verdict {
        CLOCK.with(|c| c.set(at));
        link.tick(&STA, &AP, at, expired, true, reply)
    }
    /// Looks at empty rings every 10 ms from `from` to `to`: the frames sent,
    /// and the first verdict that is neither Ignore nor Send with its time.
    fn ticks(link: &mut Link, from: u32, to: u32) -> (u32, Option<(Verdict, u32)>) {
        let mut sent = 0;
        for at in (from..=to).step_by(10) {
            match tick(link, at, &mut [0; REPLY]) {
                Verdict::Ignore => {}
                Verdict::Send(_) => sent += 1,
                other => return (sent, Some((other, at))),
            }
        }
        (sent, None)
    }

    #[test]
    fn bip_matches_the_published_broadcast_deauthentication_vector() {
        // "BIP with broadcast Deauthentication frame" from the 802.11w test
        // vectors (IEEE 802.11-08/1368, H.6.6.1; IEEE Std 802.11-2012 M.9.1):
        // the IGTK above, IPN 4, key ID 4, a broadcast Deauthentication with
        // reason 2 from 02:00:00:00:00:00, MIC 48 df bf a7 b8 27 88 72.
        // test_supplicant.py recomputes it with hostap's omac1_aes_128 and with
        // a separate Python AES-CMAC.
        let wire = hex("c0000000 ffffffffffff 020000000000 020000000000 0900 0200
                        4c10 0400 040000000000 48dfbfa7b8278872");
        let mut link = link();
        let mut reply = [0; REPLY];
        assert_eq!(link.receive(&wire, &STA, &AP, 0, &mut reply), Verdict::Disconnect);
        // The retry, power management and more-data bits are outside the MIC.
        let mut retried = wire.clone();
        retried[1] |= 0x38;
        assert!(self::link().bip(&retried));
        // The same frame again is a replay.
        assert!(!link.bip(&wire));
    }

    #[test]
    fn protected_sa_query_matches_an_independent_ccm() {
        // The expected frame comes from hostap's aes_ccm_ae with the AAD and
        // nonce built separately in test_supplicant.py (which checks it again):
        // an SA Query response under the TK above with packet number 1.
        let mut link = link();
        let mut reply = [0; REPLY];
        let request = ccmp_protect(&frame(0xd0, &AP, &STA, &[8, 0, 0x34, 0x12]), &key(TK), 7);
        assert_eq!(link.receive(&request, &STA, &AP, 0, &mut reply), Verdict::Send(44));
        assert_eq!(
            reply[..44].to_vec(),
            hex("d0400000 020000000000 020000000100 020000000000 0000
                 0100002000000000 c9d7367a 93b7955b0c12e982")
        );
    }

    #[test]
    fn without_protection_a_deauthentication_ends_the_link() {
        let mut link = Link::new();
        link.reset(false);
        let mut reply = [0; REPLY];
        for kind in [0xa0, 0xc0] {
            for to in [STA, [0xff; 6]] {
                assert_eq!(link.receive(&frame(kind, &AP, &to, &[7, 0]), &STA, &AP, 0, &mut reply), Verdict::Disconnect);
            }
        }
        // Not from the access point, not to us, or not a leave.
        let mut other = frame(0xc0, &AP, &STA, &[7, 0]);
        other[10] ^= 4;
        assert_eq!(link.receive(&other, &STA, &AP, 0, &mut reply), Verdict::Ignore);
        assert_eq!(link.receive(&frame(0xc0, &AP, &[2, 9, 9, 9, 9, 9], &[7, 0]), &STA, &AP, 0, &mut reply), Verdict::Ignore);
        assert_eq!(link.receive(&frame(0xd0, &AP, &STA, &[8, 0, 1, 2]), &STA, &AP, 0, &mut reply), Verdict::Ignore);
        // Protection negotiated but keys not installed yet: still the old rule.
        link.reset(true);
        assert!(link.active());
        assert_eq!(link.receive(&frame(0xc0, &AP, &STA, &[7, 0]), &STA, &AP, 0, &mut reply), Verdict::Disconnect);
    }

    #[test]
    fn forged_deauthentication_never_ends_a_protected_link_by_itself() {
        let mut reply = [0; REPLY];
        // Every reason but 6 and 7 is dropped: no query, no end, whatever the
        // frame type, address or claimed protection.
        for reason in (0..=0xffffu16).filter(|r| !matches!(r, 6 | 7)).step_by(7).chain([1, 2, 3, 8, 0x106, 0x0607]) {
            for (kind, to) in [(0xc0, STA), (0xa0, STA), (0xc0, [0xff; 6]), (0xa0, [0xff; 6])] {
                let mut link = link();
                let forged = frame(kind, &AP, &to, &reason.to_le_bytes());
                assert_eq!(link.receive(&forged, &STA, &AP, 100, &mut reply), Verdict::Ignore, "{reason}");
                assert!(!link.pending());
            }
        }
        // A frame that claims protection but does not decrypt is dropped too.
        let mut link = link();
        let mut claimed = frame(0xc0, &AP, &STA, &[7, 0, 0, 0x20, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        claimed[1] |= 0x40;
        assert_eq!(link.receive(&claimed, &STA, &AP, 100, &mut reply), Verdict::Ignore);
        assert!(!link.pending());
        // "Not authenticated" and "not associated" make us ask the access point.
        for reason in [6, 7] {
            for (kind, to) in [(0xc0, STA), (0xa0, STA), (0xc0, [0xff; 6]), (0xa0, [0xff; 6])] {
                let mut link = strict();
                let forged = frame(kind, &AP, &to, &[reason, 0]);
                CLOCK.with(|c| c.set(100));
                assert_eq!(link.receive(&forged, &STA, &AP, 100, &mut reply), Verdict::Send(44));
                // The query is a protected action frame that decrypts to an SA Query request.
                let mut seen = reply[..44].to_vec();
                assert_eq!((seen[0], seen[1], &seen[4..10], &seen[10..16]), (0xd0, 0x40, &AP[..], &STA[..]));
                let aes = Aes::new(&key(TK)).unwrap();
                let (aad, nonce) = ccmp_inputs(&seen, 1, true);
                let tag = ccm(&aes, &nonce, &aad, &mut seen[32..36], true);
                assert_eq!(tag.to_vec(), seen[36..44].to_vec());
                assert_eq!(seen[32..34], [8, 0]);
                let id = [seen[34], seen[35]];
                // More forgeries while it is pending send nothing more.
                assert_eq!(link.receive(&forged, &STA, &AP, 101, &mut reply), Verdict::Ignore);
                // The access point answers (protected): the link stays.
                let answer = ccmp_protect(&frame(0xd0, &AP, &STA, &[8, 1, id[0], id[1]]), &key(TK), 1);
                assert_eq!(link.receive(&answer, &STA, &AP, 150, &mut reply), Verdict::Ignore);
                assert!(!link.pending());
                assert_eq!(ticks(&mut link, 150, 9000), (0, None));
            }
        }
    }

    #[test]
    fn unanswered_sa_query_is_repeated_and_ends_a_strict_link() {
        let mut link = strict();
        let mut reply = [0; REPLY];
        assert_eq!(tick(&mut link, 0, &mut reply), Verdict::Ignore);
        CLOCK.with(|c| c.set(100));
        assert_eq!(link.receive(&frame(0xc0, &AP, &STA, &[6, 0]), &STA, &AP, 100, &mut reply), Verdict::Send(44));
        let first = reply;
        assert_eq!(tick(&mut link, 150, &mut reply), Verdict::Ignore);
        assert_eq!(tick(&mut link, 240, &mut reply), Verdict::Ignore);
        assert_eq!(tick(&mut link, 300, &mut reply), Verdict::Send(44));
        // Same transaction, a new packet number.
        assert_ne!(reply[..44], first[..44]);
        assert_eq!(reply[24], 2);
        // An answer with the wrong transaction, an unprotected one and a
        // replayed one do not count.
        let id = link.query_id.to_le_bytes();
        let wrong = ccmp_protect(&frame(0xd0, &AP, &STA, &[8, 1, id[0] ^ 1, id[1]]), &key(TK), 1);
        assert_eq!(link.receive(&wrong, &STA, &AP, 310, &mut reply), Verdict::Ignore);
        assert_eq!(link.receive(&frame(0xd0, &AP, &STA, &[8, 1, id[0], id[1]]), &STA, &AP, 311, &mut reply), Verdict::Ignore);
        let replayed = ccmp_protect(&frame(0xd0, &AP, &STA, &[8, 1, id[0], id[1]]), &key(TK), 1);
        assert_eq!(link.receive(&replayed, &STA, &AP, 312, &mut reply), Verdict::Ignore);
        assert!(link.querying);
        // Asked every 200 ms, given up a second after the first query.
        assert_eq!(ticks(&mut link, 310, 3000), (3, Some((Verdict::Disconnect, 1100))));
        assert_eq!(ticks(&mut link, 1110, 9000), (0, None));
    }

    #[test]
    fn the_second_is_not_judged_before_what_arrived_was_read() {
        let mut reply = [0; REPLY];
        let ask = |link: &mut Link, reply: &mut [u8; REPLY]| {
            CLOCK.with(|c| c.set(100));
            assert_eq!(link.receive(&frame(0xc0, &AP, &STA, &[7, 0]), &STA, &AP, 100, reply), Verdict::Send(44));
            assert_eq!(ticks(link, 110, 1050).1, None);
        };
        // The second is over, but the rings are not empty: the answer may be in
        // them. No verdict until they have been read.
        let mut link = strict();
        ask(&mut link, &mut reply);
        for at in (1100..1400).step_by(10) {
            CLOCK.with(|c| c.set(at));
            assert_ne!(link.tick(&STA, &AP, at, expired, false, &mut reply), Verdict::Disconnect);
        }
        let id = link.query_id.to_le_bytes();
        let answer = ccmp_protect(&frame(0xd0, &AP, &STA, &[8, 1, id[0], id[1]]), &key(TK), 1);
        assert_eq!(link.receive(&answer, &STA, &AP, 1400, &mut reply), Verdict::Ignore);
        assert_eq!(ticks(&mut link, 1400, 9000), (0, None));
        // Radio was away from the rings for two seconds (a blocking send, a
        // rekey): the query starts again, and a full second is given from there.
        let mut link = strict();
        ask(&mut link, &mut reply);
        assert_eq!(tick(&mut link, 3050, &mut reply), Verdict::Send(44));
        assert_eq!(ticks(&mut link, 3060, 4040).1, None);
        assert_eq!(ticks(&mut link, 4050, 9000).1, Some((Verdict::Disconnect, 4050)));
        // The same pause, answered after it: the link stays.
        let mut link = strict();
        ask(&mut link, &mut reply);
        assert_eq!(tick(&mut link, 3050, &mut reply), Verdict::Send(44));
        assert_eq!(link.receive(&answer, &STA, &AP, 3060, &mut reply), Verdict::Ignore);
        assert_eq!(ticks(&mut link, 3060, 9000), (0, None));
    }

    #[test]
    fn by_default_silence_alone_does_not_end_the_link() {
        assert!(!TIMEOUT_ENDS_LINK, "tests run on the shipped default");
        let mut reply = [0; REPLY];
        let forged = frame(0xc0, &AP, &STA, &[7, 0]);
        let forge = |link: &mut Link, at: u32, reply: &mut [u8; REPLY]| {
            CLOCK.with(|c| c.set(at));
            link.receive(&forged, &STA, &AP, at, reply)
        };
        // A forged frame and an access point that never answers the query (or
        // firmware that does not carry the exchange): the link stays, however
        // long, however often, as long as we send nothing into the silence...
        let mut link = link();
        assert_eq!(forge(&mut link, 100, &mut reply), Verdict::Send(44));
        assert_eq!(ticks(&mut link, 110, 60_000), (4, None));
        for round in 1..50 {
            assert_eq!(forge(&mut link, round * 60_000, &mut reply), Verdict::Send(44));
            assert_eq!(ticks(&mut link, round * 60_000 + 10, round * 60_000 + 59_990).1, None);
        }
        // ...and as long as the access point keeps answering what we send:
        // each unicast frame under the pairwise key clears the doubt.
        let mut link = self::link();
        assert_eq!(forge(&mut link, 100, &mut reply), Verdict::Send(44));
        for at in (1200..120_000).step_by(100) {
            link.sent();
            link.sent();
            if at % 4000 == 0 {
                link.heard();
            }
            if at % 3000 == 0 {
                forge(&mut link, at, &mut reply);
            }
            assert_eq!(ticks(&mut link, at, at + 90).1, None, "{at}");
        }
        // An access point that really lost us: eight frames sent over five
        // seconds or more and nothing came back. Not before either is true.
        let mut link = self::link();
        assert_eq!(forge(&mut link, 100, &mut reply), Verdict::Send(44));
        assert_eq!(ticks(&mut link, 110, 1090).1, None);
        assert_eq!(ticks(&mut link, 1100, 1100).1, None); // in doubt from here
        assert!(link.pending() && !link.querying);
        for _ in 0..7 {
            link.sent();
        }
        assert_eq!(ticks(&mut link, 1110, 20_000).1, None, "seven frames are too few");
        let mut link = self::link();
        assert_eq!(forge(&mut link, 100, &mut reply), Verdict::Send(44));
        assert_eq!(ticks(&mut link, 110, 1100).1, None);
        for _ in 0..200 {
            link.sent();
        }
        // Its repeated "not associated" answers to our frames do not put the end off.
        assert_eq!(ticks(&mut link, 1110, 2990).1, None);
        assert_eq!(forge(&mut link, 3000, &mut reply), Verdict::Send(44));
        assert_eq!(ticks(&mut link, 3010, 4990).1, None);
        assert_eq!(forge(&mut link, 5000, &mut reply), Verdict::Send(44));
        assert_eq!(ticks(&mut link, 5010, 6090).1, None, "five seconds are not over");
        // Frames waiting unread could be the answers: no end yet.
        CLOCK.with(|c| c.set(6100));
        assert_eq!(link.tick(&STA, &AP, 6100, expired, false, &mut reply), Verdict::Ignore);
        assert_eq!(ticks(&mut link, 6100, 9000).1, Some((Verdict::Disconnect, 6100)));
    }

    #[cfg(not(violet_hostap_aes))]
    #[test]
    fn without_a_cipher_nothing_is_trusted_and_nothing_is_left_pending() {
        let mut reply = [0; REPLY];
        let mut link = strict();
        let unicast = ccmp_protect(&frame(0xc0, &AP, &STA, &[3, 0]), &key(TK), 9);
        let broadcast = bip_protect(frame(0xc0, &AP, &[0xff; 6], &[3, 0]), &key(IGTK), 4, 4);
        cipher::EXHAUSTED.with(|e| e.set(true));
        // Authentic frames cannot be checked: they do not end the link.
        assert_eq!(link.receive(&unicast, &STA, &AP, 0, &mut reply), Verdict::Ignore);
        assert_eq!(link.receive(&broadcast, &STA, &AP, 0, &mut reply), Verdict::Ignore);
        // A query cannot be built: none is started, so none can time out.
        assert_eq!(link.receive(&frame(0xc0, &AP, &STA, &[7, 0]), &STA, &AP, 0, &mut reply), Verdict::Ignore);
        assert!(!link.pending());
        assert_eq!(ticks(&mut link, 0, 5000), (0, None));
        cipher::EXHAUSTED.with(|e| e.set(false));
        // With the cipher back the same frames count, packet numbers untouched.
        assert_eq!(link.receive(&unicast, &STA, &AP, 0, &mut reply), Verdict::Disconnect);
        assert_eq!(link.receive(&broadcast, &STA, &AP, 0, &mut reply), Verdict::Disconnect);
    }

    #[test]
    fn authentic_deauthentication_ends_the_link() {
        let mut reply = [0; REPLY];
        for kind in [0xa0, 0xc0] {
            let unicast = ccmp_protect(&frame(kind, &AP, &STA, &[3, 0]), &key(TK), 9);
            assert_eq!(link().receive(&unicast, &STA, &AP, 0, &mut reply), Verdict::Disconnect);
            let broadcast = bip_protect(frame(kind, &AP, &[0xff; 6], &[3, 0]), &key(IGTK), 4, 4);
            assert_eq!(link().receive(&broadcast, &STA, &AP, 0, &mut reply), Verdict::Disconnect);
        }
    }

    #[test]
    fn tampered_replayed_or_wrongly_keyed_frames_are_not_authentic() {
        let mut reply = [0; REPLY];
        let unicast = ccmp_protect(&frame(0xc0, &AP, &STA, &[3, 0]), &key(TK), 9);
        // Every single-bit change to a protected deauthentication is rejected
        // (duration and sequence number are outside the MIC by design).
        for byte in (0..unicast.len()).filter(|i| !matches!(i, 2 | 3 | 22 | 23 | 26)) {
            // Byte 26 and the low bits of byte 27 of the CCMP header are reserved.
            for bit in 0..8 {
                if byte == 1 && matches!(bit, 3 | 4 | 5) || byte == 27 && bit < 5 {
                    continue;
                }
                let mut bad = unicast.clone();
                bad[byte] ^= 1 << bit;
                let verdict = link().receive(&bad, &STA, &AP, 0, &mut reply);
                assert_ne!(verdict, Verdict::Disconnect, "byte {byte} bit {bit}");
            }
        }
        let mut link = link();
        assert!(link.open(&unicast, &mut [0; PLAIN]).is_some());
        assert!(link.open(&unicast, &mut [0; PLAIN]).is_none(), "replay");
        let older = ccmp_protect(&frame(0xc0, &AP, &STA, &[3, 0]), &key(TK), 8);
        assert!(link.open(&older, &mut [0; PLAIN]).is_none(), "old packet number");
        let other_key = ccmp_protect(&frame(0xc0, &AP, &STA, &[3, 0]), &key(IGTK), 10);
        assert!(link.open(&other_key, &mut [0; PLAIN]).is_none());
        // A rekey restarts the packet numbers.
        link.set_tk(&key(TK));
        assert!(link.open(&older, &mut [0; PLAIN]).is_some());

        let broadcast = bip_protect(frame(0xc0, &AP, &[0xff; 6], &[3, 0]), &key(IGTK), 4, 4);
        for byte in (0..broadcast.len()).filter(|i| !matches!(i, 2 | 3 | 22 | 23)) {
            for bit in 0..8 {
                if byte == 1 && matches!(bit, 3 | 4 | 5) {
                    continue;
                }
                let mut bad = broadcast.clone();
                bad[byte] ^= 1 << bit;
                assert!(!self::link().bip(&bad), "byte {byte} bit {bit}");
            }
        }
        let mut link = self::link();
        assert!(!link.bip(&bip_protect(frame(0xc0, &AP, &[0xff; 6], &[3, 0]), &key(IGTK), 4, 3)), "packet number not above the key's");
        assert!(!link.bip(&bip_protect(frame(0xc0, &AP, &[0xff; 6], &[3, 0]), &key(IGTK), 5, 9)), "key 5 is not installed");
        assert!(!link.bip(&bip_protect(frame(0xc0, &AP, &[0xff; 6], &[3, 0]), &key(TK), 4, 9)), "wrong key");
        assert!(!link.bip(&frame(0xc0, &AP, &[0xff; 6], &[3, 0])), "no MME");
        assert!(link.bip(&broadcast));
        // The second IGTK, after a group rekey, with its own packet number.
        assert!(link.set_igtk(5, &key(TK), &[0x10, 0, 0, 0, 0, 0]));
        assert!(!link.bip(&bip_protect(frame(0xa0, &AP, &[0xff; 6], &[3, 0]), &key(TK), 5, 0x10)));
        assert!(link.bip(&bip_protect(frame(0xa0, &AP, &[0xff; 6], &[3, 0]), &key(TK), 5, 0x11)));
    }

    #[test]
    fn keys_are_refused_without_protection_and_forgotten_on_a_new_association() {
        let mut link = Link::new();
        assert!(!link.set_igtk(4, &key(IGTK), &[0; 6]), "protection not negotiated");
        link.reset(true);
        for (id, key_len, ipn_len) in [(3, 16, 6), (6, 16, 6), (4, 32, 6), (4, 16, 8)] {
            assert!(!link.set_igtk(id, &[1; 32][..key_len], &[0; 8][..ipn_len]));
        }
        // A new association and the end of a link both wipe them.
        for active in [true, false] {
            let mut link = self::link();
            link.timeout_ends = true;
            link.reset(active);
            assert_eq!((link.tk, link.igtk, link.keyed, link.igtk_set), ([0; 16], [[0; 16]; 2], false, [false; 2]));
            assert_eq!((link.active, link.pending(), link.timeout_ends), (active, false, true));
        }
    }

    #[test]
    fn answers_the_access_points_sa_query_only_when_it_is_protected() {
        let mut link = link();
        let mut reply = [0; REPLY];
        assert_eq!(link.receive(&frame(0xd0, &AP, &STA, &[8, 0, 0x55, 0xaa]), &STA, &AP, 0, &mut reply), Verdict::Ignore);
        let request = ccmp_protect(&frame(0xd0, &AP, &STA, &[8, 0, 0x55, 0xaa]), &key(TK), 1);
        assert_eq!(link.receive(&request, &STA, &AP, 0, &mut reply), Verdict::Send(44));
        let mut body = reply[32..36].to_vec();
        let (aad, nonce) = ccmp_inputs(&reply[..24], 1, true);
        let tag = ccm(&Aes::new(&key(TK)).unwrap(), &nonce, &aad, &mut body, true);
        assert_eq!((body, tag.to_vec()), (vec![8, 1, 0x55, 0xaa], reply[36..44].to_vec()));
        // Truncated frames of every length are handled without a panic.
        for n in 0..request.len() {
            let _ = link.receive(&request[..n], &STA, &AP, 0, &mut reply);
        }
        let deauth = bip_protect(frame(0xc0, &AP, &[0xff; 6], &[3, 0]), &key(IGTK), 4, 9);
        for n in 0..deauth.len() {
            assert_ne!(self::link().receive(&deauth[..n], &STA, &AP, 0, &mut reply), Verdict::Disconnect);
        }
    }
}
