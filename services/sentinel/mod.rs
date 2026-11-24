//! Isolated seL4 `Sentinel` service — VioletOS's tamper-resistant observability
//! and signed-Shield verification daemon (Shirudo's runtime/attestation analog).
//!
//! Runs in its own seL4 TCB & address space, completely inaccessible to open apps:
//! - Reads `TelemetryEvent`s from the one-way shared ring written by `OpenRuntime`.
//! - Buffers events locally in `OfflineEventRing` when the iPhone relay is disconnected.
//! - Maintains a boot-to-now SHA-256 `MeasurementChain` (Shirudo TPM PCR analog)
//!   covering the boot catalog, `OpenRuntime`, and every Shield load/unload.
//! - Connects to `sentinel.violet` on the iPhone companion (or Mac fallback on home Wi-Fi).
//! - Verifies P-256 ECDSA signatures (`VSH1`) via BearSSL against the paired iPhone
//!   Secure Enclave public key (reusing `supervisor.c`'s P-256 verifier) before
//!   installing a Shield into `OpenRuntime`, with automatic trial-and-rollback.

/// Wire magic for signed Violet Shield envelopes (`"VSH1"`).
pub const SHIELD_MAGIC: [u8; 4] = *b"VSH1";

/// Signing tier distinguishing automated local containment from permanent policies.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigningTier {
    /// iPhone companion key: permitted to push observability probes and reversible,
    /// TTL-bounded containment shields (e.g. max 1 hour `DENY` or `FREEZE_APP`).
    CompanionAuto = 1,
    /// Hardware-gated owner key (iPhone Face ID Secure Enclave or Mac developer key):
    /// required for permanent rules (`ttl_secs == 0`) or system-wide policies.
    OwnerConfirmed = 2,
}

/// Fixed header of a signed Violet Shield pushed over `sentinel.violet`.
/// Signature covers `magic .. payload_len + payload` using P-256 ECDSA (SHA-256),
/// matching `br_ecdsa_i31_vrfy_asn1` in `system/supervisor/supervisor.c`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SignedShieldHeader {
    pub magic: [u8; 4],        // "VSH1"
    pub version: u16,          // 1
    pub tier: u8,              // SigningTier
    pub op: u8,                // 1 = install, 2 = revoke, 3 = unfreeze_app, 4 = kill_app
    pub shield_id: u32,
    pub seq_no: u64,           // monotonic anti-replay sequence number
    pub ttl_secs: u32,         // mandatory non-zero cap for CompanionAuto
    pub payload_len: u32,
    pub sig_len: u16,          // ASN.1 DER P-256 signature length (<= 72)
    pub _reserved: u16,
    pub signature: [u8; 72],
}

/// Shirudo-style per-boot measurement chain (software PCR analog when no discrete
/// TPM 2.0 is exposed to seL4 on Moto G Pure).
///
/// Extended on boot with the verified catalog digest, and on every Shield load/unload:
///   `chain_new = SHA256(chain_old || event_tag || digest)`
#[derive(Clone, Copy, Debug)]
pub struct MeasurementChain {
    pub boot_id: [u8; 16],      // random per-boot nonce from hardware/virtio RNG
    pub seq: u64,               // incremented on every state transition or heartbeat
    pub chain_digest: [u8; 32], // running SHA-256 PCR value
    pub active_shield_count: u16,
}

impl MeasurementChain {
    pub fn extend(&mut self, _tag: u8, _item_digest: &[u8; 32]) {
        self.seq = self.seq.wrapping_add(1);
        // TODO: BearSSL br_sha256_init/update(chain_digest, tag, item_digest)/out.
    }
}

/// Signed heartbeat sent periodically over `sentinel.violet` so the iPhone checker
/// immediately detects silent telemetry loss, shield rollbacks, or unexpected reboots.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AttestedHeartbeat {
    pub boot_id: [u8; 16],
    pub uptime_ms: u64,
    pub seq: u64,
    pub chain_digest: [u8; 32],
    pub dropped_events: u32,
    pub active_shields: u16,
    pub frozen_apps: u16,
}

/// Trial-and-rollback safety guard (mirroring `usb_trial_timeout_ms` in `supervisor.c`).
/// If a newly installed Shield breaks `Sentinel`'s own heartbeat or triggers a fault
/// before `confirm_deadline_ms`, `Sentinel` automatically revokes it.
#[derive(Clone, Copy, Debug)]
pub struct PendingTrial {
    pub shield_id: u32,
    pub confirm_deadline_ms: u64,
}

/// Core `Sentinel` service interface wired to `OpenRuntime`, `Supervisor`, and `Radio`.
pub trait SentinelService {
    /// Drain the one-way ring from `OpenRuntime`, drop local noise, and enqueue
    /// high-signal transitions into `OfflineEventRing`.
    fn poll_telemetry_ring(&mut self, now_ms: u64);

    /// Verify P-256 signature, anti-replay `seq_no`, and `SigningTier` TTL bounds,
    /// extend `MeasurementChain`, and push the rule into `OpenRuntime::ShieldPolicy`.
    fn verify_and_apply_shield(
        &mut self,
        header: &SignedShieldHeader,
        payload: &[u8],
        now_ms: u64,
    ) -> Result<(), SentinelErr>;

    /// Request Supervisor to suspend (`seL4_TCB_Suspend`) or resume an app mid-syscall.
    fn set_app_frozen(&mut self, app_id: u32, frozen: bool) -> Result<(), SentinelErr>;

    /// Respond to a read-only forensic query from the iPhone checker (e.g., SHA-256
    /// or bounded snippet of a newly dropped `.py` script, open FD list, or VMA map).
    fn handle_forensic_query(&mut self, app_id: u32, query_kind: u8) -> Result<usize, SentinelErr>;

    /// Flush buffered events and `AttestedHeartbeat` over `sentinel.violet` when online.
    fn sync_companion(&mut self, now_ms: u64);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentinelErr {
    BadMagic,
    BadSignature,
    ReplaySequence,
    TtlRequiredForAutoTier,
    RuleTableFull,
    CompanionOffline,
}
