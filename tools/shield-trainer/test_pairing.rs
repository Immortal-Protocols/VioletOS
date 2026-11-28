//! Host tests for VSP1 companion pairing (services/sentinel/pairing.rs) with
//! the Sentinel's real SHA-256 and P-256 verifier (services/sentinel/crypto.c
//! on BearSSL) against messages made by the reference companion
//! (tools/shield-trainer/pairing.py, its own ECDSA). Run by
//! tools/shield-trainer/test_pairing.sh, part of `make test-unit`.
#[path = "../../services/sentinel/pairing.rs"]
mod pairing;

use pairing::{Crypto, Enrolled, Pairing, Refused};

extern "C" {
    fn sentinel_sha256(data: *const u8, len: usize, out: *mut u8);
    fn sentinel_p256_verify(key: *const u8, hash: *const u8, sig: *const u8, sig_len: usize) -> i32;
}

struct Bear;
impl Crypto for Bear {
    fn sha256(&self, data: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        unsafe { sentinel_sha256(data.as_ptr(), data.len(), out.as_mut_ptr()) };
        out
    }
    fn p256_verify(&self, key: &[u8; 65], digest: &[u8; 32], sig: &[u8]) -> bool {
        unsafe { sentinel_p256_verify(key.as_ptr(), digest.as_ptr(), sig.as_ptr(), sig.len()) == 1 }
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

struct Vectors {
    random: [u8; 32],
    key: [u8; 65],
    record: Vec<u8>,
    /// name, the phone's answer, ENROLL, CONFIRM
    cases: Vec<(String, String, Vec<u8>, Vec<u8>)>,
}

fn vectors() -> Vectors {
    let text = std::fs::read_to_string(std::env::var("VSP1_FIXTURES").expect("VSP1_FIXTURES (test_pairing.sh sets it)")).unwrap();
    let mut v = Vectors { random: [0; 32], key: [0; 65], record: Vec::new(), cases: Vec::new() };
    for line in text.lines() {
        let p: Vec<&str> = line.split(' ').collect();
        match p[0] {
            "random" => v.random = hex(p[1]).try_into().unwrap(),
            "key" => v.key = hex(p[1]).try_into().unwrap(),
            "record" => v.record = hex(p[1]),
            "case" => v.cases.push((p[1].into(), p[2].into(), hex(p[3]), if p[4] == "-" { Vec::new() } else { hex(p[4]) })),
            _ => {}
        }
    }
    assert!(v.cases.len() >= 8 && v.record.len() == pairing::RECORD_LEN);
    v
}

fn armed(v: &Vectors, now: u64) -> Pairing {
    let mut p = Pairing::new();
    let offer = p.arm(now, &v.random).unwrap();
    assert_eq!(&offer[..6], b"VSP1\x01\x00");
    assert_eq!(offer[6..], v.random, "the offer is the nonce and the secret");
    p
}
fn case<'a>(v: &'a Vectors, name: &str) -> &'a [u8] {
    &v.cases.iter().find(|c| c.0 == name).unwrap().2
}

#[test]
fn every_reference_case_gets_the_specified_answer() {
    let v = vectors();
    for (name, want, msg, confirm) in &v.cases {
        let mut p = armed(&v, 1_000);
        let got = p.enroll(&Bear, 2_000, msg);
        let said = match &got {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("{:?}", e),
        };
        assert_eq!(&said, want, "{}", name);
        if let Ok((enrolled, sent)) = got {
            assert_eq!(enrolled, Enrolled { key: v.key, kind: pairing::KIND_SOFTWARE, tier: pairing::TIER_AUTO, last_seq: 0 });
            assert_eq!(&sent[..], &confirm[..], "the confirmation the companion's reference expects");
            assert_eq!(sent[7..15], Bear.sha256(&v.key)[..8]);
        }
    }
    assert_eq!(v.cases.iter().filter(|c| c.1 == "ok").count(), 1);
}

#[test]
fn pairing_needs_the_user_a_fresh_secret_and_few_tries() {
    let v = vectors();
    let good = case(&v, "good");
    // Never started on the phone.
    assert_eq!(Pairing::new().enroll(&Bear, 5, good).unwrap_err(), Refused::NotArmed);
    // The secret on the screen is good for two minutes.
    let mut p = armed(&v, 1_000);
    assert!(p.armed(1_000 + pairing::WINDOW_NS - 1) && !p.armed(1_000 + pairing::WINDOW_NS));
    assert_eq!(p.enroll(&Bear, 1_000 + pairing::WINDOW_NS, good).unwrap_err(), Refused::NotArmed);
    assert_eq!(p.enroll(&Bear, 2_000, good).unwrap_err(), Refused::NotArmed, "and stays over once it lapsed");
    // Three wrong guesses end it, even for the right companion.
    let mut p = armed(&v, 1_000);
    for _ in 0..pairing::ATTEMPTS {
        assert_eq!(p.enroll(&Bear, 2_000, case(&v, "wrong-secret")).unwrap_err(), Refused::BadMac);
    }
    assert_eq!(p.enroll(&Bear, 2_000, good).unwrap_err(), Refused::NotArmed);
    // Garbage does not use up tries; a forged signature does.
    let mut p = armed(&v, 1_000);
    for _ in 0..10 {
        assert_eq!(p.enroll(&Bear, 2_000, case(&v, "truncated")).unwrap_err(), Refused::Malformed);
        assert_eq!(p.enroll(&Bear, 2_000, case(&v, "owner-tier")).unwrap_err(), Refused::TierNotAllowed);
        assert_eq!(p.enroll(&Bear, 2_000, b"VSPE").unwrap_err(), Refused::Malformed);
    }
    assert_eq!(p.enroll(&Bear, 2_000, case(&v, "not-the-holder")).unwrap_err(), Refused::BadSignature);
    assert_eq!(p.enroll(&Bear, 2_000, case(&v, "off-curve")).unwrap_err(), Refused::BadSignature);
    assert!(p.enroll(&Bear, 2_000, good).is_ok(), "the third try");
    // One key per pairing: the same message again is refused.
    assert_eq!(p.enroll(&Bear, 2_000, good).unwrap_err(), Refused::NotArmed);
    // A new pairing has a new secret: yesterday's message does not fit it.
    let mut other = v.random;
    other[20] ^= 0x55;
    let mut p = Pairing::new();
    p.arm(1_000, &other).unwrap();
    assert_eq!(p.enroll(&Bear, 2_000, good).unwrap_err(), Refused::BadMac);
    let mut nonce = v.random;
    nonce[3] ^= 0x55;
    p.arm(1_000, &nonce).unwrap();
    assert_eq!(p.enroll(&Bear, 2_000, good).unwrap_err(), Refused::BadMac, "nor one made for another nonce");
    // Bytes that are plainly not random are not shown at all.
    assert!(p.arm(1_000, &[0; 32]).is_none() && p.arm(1_000, &[7; 32]).is_none());
    assert!(!p.armed(1_001));
    // Other versions and kinds.
    let mut p = armed(&v, 1_000);
    let mut m = good.to_vec();
    m[4] = 2;
    assert_eq!(p.enroll(&Bear, 2_000, &m).unwrap_err(), Refused::Version);
    let mut m = good.to_vec();
    m[6] = 9;
    assert_eq!(p.enroll(&Bear, 2_000, &m).unwrap_err(), Refused::Malformed);
    assert_eq!(p.enroll(&Bear, 2_000, case(&v, "claims-attestation")).unwrap_err(), Refused::AttestationUnsupported);
    let mut m = good.to_vec();
    m.extend_from_slice(&[0; 80]);
    assert_eq!(p.enroll(&Bear, 2_000, &m).unwrap_err(), Refused::Malformed);
}

#[test]
fn the_stored_key_keeps_its_limits_and_its_sequence_number() {
    let v = vectors();
    let (mut e, _) = armed(&v, 1).enroll(&Bear, 2, case(&v, "good")).unwrap();
    // A software key signs automatic-tier commands only.
    assert!(e.allows(pairing::TIER_AUTO) && !e.allows(pairing::TIER_OWNER) && !e.allows(0) && !e.allows(3));
    let forged = Enrolled { tier: pairing::TIER_OWNER, ..e };
    assert!(!forged.allows(pairing::TIER_OWNER), "owner authority needs an attested key, whatever the record says");
    // Anti-replay, across a save and a reload.
    assert!(e.advance(5) && e.advance(7) && !e.advance(7) && !e.advance(6) && !e.advance(0));
    let saved = e.record(&Bear);
    assert_eq!(saved[..], v.record[..], "the record the reference writes");
    let mut back = Enrolled::parse(&Bear, &saved).unwrap();
    assert_eq!(back, e);
    assert!(!back.advance(7) && back.advance(8));
    // Anything that is not exactly a record leaves the phone unpaired.
    for n in 0..saved.len() {
        let mut bad = saved;
        bad[n] ^= 1;
        assert!(Enrolled::parse(&Bear, &bad).is_none(), "byte {}", n);
    }
    assert!(Enrolled::parse(&Bear, &saved[..saved.len() - 1]).is_none() && Enrolled::parse(&Bear, &[]).is_none());
    // A well-formed record claiming more than enrolment can give is refused too.
    for (kind, tier) in [(pairing::KIND_ATTESTED, pairing::TIER_AUTO), (pairing::KIND_SOFTWARE, pairing::TIER_OWNER), (pairing::KIND_ATTESTED, pairing::TIER_OWNER)] {
        let claim = Enrolled { kind, tier, ..e }.record(&Bear);
        assert!(Enrolled::parse(&Bear, &claim).is_none());
    }
}
