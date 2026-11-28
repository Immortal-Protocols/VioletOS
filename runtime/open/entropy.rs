//! Random numbers for the Open Runtime and its apps: getrandom, /dev/urandom,
//! AT_RANDOM and the address-space layout all draw from one generator.
//!
//! The generator is HMAC_DRBG (NIST SP 800-90A) over SHA-256; the hash is
//! passed in, so the image uses the SHA-256 it already links and the host
//! tests (tests/memprot_host.rs) check the standard's known answers. It gives
//! nothing until it is seeded: callers fail (EIO, an app does not start)
//! rather than hand out predictable bytes.
//!
//! Seed: `collect` samples the timer across yields to the other processes
//! (timing jitter) until a deliberately low estimate (`Jitter`) says the
//! samples hold 256 bits, and leaves the generator unseeded if they never do
//! or if the hash fails its known answer. Every system call's arrival time
//! is stirred in afterwards. `reseed` takes bytes from a hardware source; no
//! such source reaches this process yet (README.md, "Memory protection"), so
//! the seed is only as good as the timing jitter the estimate counted.

pub type Sha256 = fn(&[u8]) -> [u8; 32];

/// The most seed or stirred input one update takes as is; longer is hashed first.
const INPUT: usize = 128;

pub struct Drbg {
    key: [u8; 32],
    v: [u8; 32],
    sha: Sha256,
    seeded: bool,
    /// Event timings waiting to go in with the next request.
    pool: [u32; 4],
    stirred: u32,
    /// Bytes came from a hardware source, not timing alone.
    pub hardware: bool,
}

impl Drbg {
    /// Not seeded: `fill` refuses until `seed` or `reseed`.
    pub fn new(sha: Sha256) -> Drbg {
        Drbg { key: [0; 32], v: [1; 32], sha, seeded: false, pool: [0; 4], stirred: 0, hardware: false }
    }

    fn hmac(&self, parts: &[&[u8]]) -> [u8; 32] {
        let mut inner = [0x36u8; 64 + 32 + 1 + INPUT];
        let mut outer = [0x5cu8; 64 + 32];
        for i in 0..32 {
            inner[i] ^= self.key[i];
            outer[i] ^= self.key[i];
        }
        let mut n = 64;
        for part in parts {
            inner[n..n + part.len()].copy_from_slice(part);
            n += part.len();
        }
        outer[64..].copy_from_slice(&(self.sha)(&inner[..n]));
        (self.sha)(&outer)
    }

    /// HMAC_DRBG_Update.
    fn update(&mut self, data: &[u8]) {
        let digest;
        let data = if data.len() > INPUT {
            digest = (self.sha)(data);
            &digest[..]
        } else {
            data
        };
        for round in 0..2u8 {
            self.key = self.hmac(&[&self.v, &[round], data]);
            self.v = self.hmac(&[&self.v]);
            if data.is_empty() {
                break;
            }
        }
    }

    /// Instantiate or reseed with entropy the caller vouches for.
    pub fn seed(&mut self, entropy: &[u8]) {
        self.update(entropy);
        self.seeded = true;
    }

    /// Mix in bytes from a hardware random source.
    pub fn reseed(&mut self, entropy: &[u8]) {
        self.seed(entropy);
        self.hardware = true;
    }

    pub fn seeded(&self) -> bool {
        self.seeded
    }

    /// Note when something happened (a counter value). Cheap; the timings go
    /// into the generator with the next request. Never counts as a seed.
    pub fn stir(&mut self, t: u32) {
        let i = (self.stirred & 3) as usize;
        self.pool[i] = self.pool[i].rotate_left(7) ^ t;
        self.stirred = self.stirred.wrapping_add(1);
    }

    /// Fill `out` with random bytes. False, and `out` untouched, if the
    /// generator was never seeded.
    pub fn fill(&mut self, out: &mut [u8]) -> bool {
        if !self.seeded {
            return false;
        }
        if self.stirred != 0 {
            let mut input = [0u8; 16];
            for (i, w) in self.pool.iter().enumerate() {
                input[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            self.update(&input);
            self.stirred = 0;
        }
        for chunk in out.chunks_mut(32) {
            self.v = self.hmac(&[&self.v]);
            chunk.copy_from_slice(&self.v[..chunk.len()]);
        }
        // A new key after every request: what was given out cannot be recomputed.
        self.update(&[]);
        true
    }
}

/// A low estimate of the randomness in a run of timer samples. A sample
/// counts only if the step to it differs by two ticks or more from each of
/// the eight steps before it. So nothing is counted for a timer that is
/// stuck, moves by a constant step, alternates or repeats with a period up
/// to eight, drifts slowly, or varies by one tick (a fixed delay rounded to
/// whole ticks). Each sample that counts is taken as half a bit: of
/// independent steps whose likeliest value has probability p, at most
/// 2(1 - p) count, so the credit stays under 0.7 of their min-entropy.
#[derive(Default)]
pub struct Jitter {
    last: u32,
    steps: [u32; LAGS],
    pub samples: u32,
    counted: u32,
}

const LAGS: usize = 8;

impl Jitter {
    pub fn sample(&mut self, t: u32) {
        let step = t.wrapping_sub(self.last);
        self.last = t;
        // The first sample has no step, and the next eight nothing to compare with.
        if self.samples > LAGS as u32 && self.steps.iter().all(|&s| step.wrapping_sub(s).wrapping_add(1) > 2) {
            self.counted += 1;
        }
        self.steps[self.samples as usize % LAGS] = step;
        self.samples += 1;
    }
    /// Bits of randomness credited to the samples so far.
    pub fn bits(&self) -> u32 {
        self.counted / 2
    }
}

/// Bits a seed must be credited with, and the most samples taken looking for them.
pub const SEED_BITS: u32 = 256;
pub const MAX_SAMPLES: u32 = 32768;

/// Seed `drbg` from timer samples (`next` gives one after letting something
/// else happen): every sample goes into the generator, and it counts as
/// seeded once `Jitter` credits them with SEED_BITS. If MAX_SAMPLES do not
/// get there, or the hash is not SHA-256, it is left as it was. Returns the
/// samples taken and the bits credited.
pub fn gather(drbg: &mut Drbg, next: &mut dyn FnMut() -> u32) -> (u32, u32) {
    // SHA-256("abc"): a wrong hash would make every output guessable.
    if (drbg.sha)(b"abc")[..8] != [0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea] {
        return (0, 0);
    }
    let mut jitter = Jitter::default();
    let mut block = [0u8; INPUT];
    while jitter.samples < MAX_SAMPLES {
        for at in (0..INPUT).step_by(4) {
            let t = next();
            jitter.sample(t);
            block[at..at + 4].copy_from_slice(&t.to_le_bytes());
        }
        drbg.update(&block);
        if jitter.bits() >= SEED_BITS {
            drbg.seeded = true;
            break;
        }
    }
    (jitter.samples, jitter.bits())
}

// `collect` and `boot`, which read this process's timer, are in sys.rs:
// this file is also built into the 64-bit bootstrap task (targets/qemu64).
