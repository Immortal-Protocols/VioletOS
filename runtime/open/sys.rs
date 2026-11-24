//! seL4 invocations (through sel4.c), the capability layout the supervisor
//! hands the Open Runtime, and its fixed virtual memory map.

extern "C" {
    fn open_retype(untyped: u32, kind: u32, bits: u32, root: u32, slot: u32) -> i32;
    fn open_object_type(which: u32) -> u32;
    fn open_page_map(frame: u32, pd: u32, va: u32, writable: i32, cached: i32) -> i32;
    fn open_page_unmap(frame: u32) -> i32;
    fn open_page_unify(frame: u32, bytes: u32) -> i32;
    fn open_pt_map(pt: u32, pd: u32, va: u32) -> i32;
    fn open_asid_assign(pool: u32, pd: u32) -> i32;
    fn open_cnode_copy(root: u32, dest: u32, bits: u32, src_root: u32, src: u32, src_bits: u32) -> i32;
    fn open_cnode_mint(root: u32, dest: u32, bits: u32, src_root: u32, src: u32, src_bits: u32, badge: u32) -> i32;
    fn open_cnode_delete(root: u32, slot: u32, bits: u32) -> i32;
    fn open_cnode_revoke(root: u32, slot: u32, bits: u32) -> i32;
    fn open_save_caller(root: u32, slot: u32, bits: u32) -> i32;
    fn open_tcb_configure(tcb: u32, fault_ep: u32, cspace: u32, cspace_bits: u32, vspace: u32) -> i32;
    fn open_tcb_priority(tcb: u32, authority: u32, priority: u32) -> i32;
    fn open_tcb_write(tcb: u32, resume: i32, regs: *const u32) -> i32;
    fn open_tcb_read(tcb: u32, regs: *mut u32) -> i32;
    fn open_tcb_set_tls(tcb: u32, base: u32) -> i32;
    fn open_tcb_suspend(tcb: u32) -> i32;
    fn open_tcb_resume(tcb: u32) -> i32;
    fn open_receive(ep: u32, block: i32, badge: *mut u32, mrs: *mut u32) -> u32;
    fn open_reply(mrs: *const u32, length: u32);
    fn open_send(cap: u32, mrs: *const u32, length: u32);
    fn open_yield();
    fn open_clean(pd: u32, start: u32, end: u32) -> i32;
}

/// Write the rows y0..y1 of scanout (mapped cached) back to memory, so the
/// display engine sees them. Cleaned one 64 KiB frame at a time.
pub fn clean_framebuffer(y0: usize, y1: usize) {
    let stride = 720 * 4;
    let (mut a, end) = (FRAMEBUFFER + y0 * stride, FRAMEBUFFER + y1.min(1600) * stride);
    while a < end {
        let next = ((a & !0xffff) + 0x10000).min(end);
        unsafe { open_clean(SLOT_PD, a as u32, next as u32) };
        a = next;
    }
}

/// Capability slots the supervisor fills (see open_runtime() in supervisor.c).
pub const SLOT_RUNTIME: u32 = 2;
pub const SLOT_CNODE: u32 = 3;
pub const SLOT_PD: u32 = 4;
pub const SLOT_ASID: u32 = 5;
pub const SLOT_KERNEL: u32 = 6;
pub const SLOT_AUTHORITY: u32 = 7;
pub const SLOT_POOL: u32 = 16;
/// Slots this process fills itself.
pub const SLOT_FAULT_EP: u32 = 8;
pub const CNODE_BITS: u32 = 14;
pub const SLOTS: u32 = 1 << CNODE_BITS;
/// First slot handed out dynamically.
pub const SLOT_DYNAMIC: u32 = 64;

/// Virtual memory map of the Open Runtime itself.
pub const FRAMEBUFFER: usize = 0x0800_0000;
pub const LOG_PAGE: usize = 0x0900_0000;
pub const INITFS: usize = 0x1000_0000;
pub const INITFS_BYTES: usize = 0x0600_0000;
pub const POOL_VA: usize = 0x2000_0000;

/// Physical app memory: the sizes (as bit counts) of the untypeds the
/// supervisor places at SLOT_POOL onwards, per board.
#[cfg(not(moto_board))]
pub const POOL_BITS: &[u32] = &[27, 28];
#[cfg(moto_board)]
pub const POOL_BITS: &[u32] = &[28];
pub const PAGE_BITS: u32 = 16;
pub const PAGE: usize = 1 << PAGE_BITS;
pub const fn pool_pages() -> usize {
    let mut total = 0;
    let mut i = 0;
    while i < POOL_BITS.len() {
        total += 1 << (POOL_BITS[i] - PAGE_BITS);
        i += 1;
    }
    total
}
pub const POOL_PAGES: usize = pool_pages();

pub const PRIORITY: u32 = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Object {
    Untyped,
    Tcb,
    Endpoint,
    CNode,
    PageDirectory,
    PageTable,
    LargePage,
    SmallPage,
}
pub fn object_type(o: Object) -> u32 {
    unsafe { open_object_type(o as u32) }
}

pub fn retype(untyped: u32, o: Object, bits: u32, slot: u32) -> Result<(), i32> {
    check(unsafe { open_retype(untyped, object_type(o), bits, SLOT_CNODE, slot) })
}
fn check(e: i32) -> Result<(), i32> {
    if e == 0 { Ok(()) } else { Err(e) }
}
pub fn page_map(frame: u32, pd: u32, va: u32, writable: bool) -> Result<(), i32> {
    check(unsafe { open_page_map(frame, pd, va, writable as i32, 1) })
}
pub fn device_map(frame: u32, pd: u32, va: u32) -> Result<(), i32> {
    check(unsafe { open_page_map(frame, pd, va, 1, 0) })
}
pub fn page_unmap(frame: u32) {
    unsafe { open_page_unmap(frame) };
}
/// Make instructions written through another mapping visible to execution.
pub fn page_unify(frame: u32) {
    unsafe { open_page_unify(frame, PAGE as u32) };
}
pub fn pt_map(pt: u32, pd: u32, va: u32) -> Result<(), i32> {
    check(unsafe { open_pt_map(pt, pd, va) })
}
pub fn asid_assign(pd: u32) -> Result<(), i32> {
    check(unsafe { open_asid_assign(SLOT_ASID, pd) })
}
pub fn copy(dest: u32, src: u32) -> Result<(), i32> {
    check(unsafe { open_cnode_copy(SLOT_CNODE, dest, CNODE_BITS, SLOT_CNODE, src, CNODE_BITS) })
}
/// Copy into another CNode (an app's), at `slot` of `bits` depth.
pub fn mint_into(cnode: u32, slot: u32, bits: u32, src: u32, badge: u32) -> Result<(), i32> {
    check(unsafe { open_cnode_mint(cnode, slot, bits, SLOT_CNODE, src, CNODE_BITS, badge) })
}
pub fn delete(slot: u32) {
    unsafe { open_cnode_delete(SLOT_CNODE, slot, CNODE_BITS) };
}
pub fn revoke(slot: u32) {
    unsafe { open_cnode_revoke(SLOT_CNODE, slot, CNODE_BITS) };
}
pub fn save_caller(slot: u32) -> Result<(), i32> {
    check(unsafe { open_save_caller(SLOT_CNODE, slot, CNODE_BITS) })
}
pub fn tcb_configure(tcb: u32, fault_ep: u32, cspace: u32, cspace_bits: u32, vspace: u32) -> Result<(), i32> {
    check(unsafe { open_tcb_configure(tcb, fault_ep, cspace, cspace_bits, vspace) })?;
    check(unsafe { open_tcb_priority(tcb, SLOT_AUTHORITY, PRIORITY) })
}

/// seL4_UserContext on AArch32.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Regs {
    pub pc: u32,
    pub sp: u32,
    pub cpsr: u32,
    pub r0: u32,
    pub r1: u32,
    pub r8: u32,
    pub r9: u32,
    pub r10: u32,
    pub r11: u32,
    pub r12: u32,
    pub r2: u32,
    pub r3: u32,
    pub r4: u32,
    pub r5: u32,
    pub r6: u32,
    pub r7: u32,
    pub r14: u32,
    pub tpidrurw: u32,
    pub tpidruro: u32,
}
pub fn tcb_write(tcb: u32, resume: bool, regs: &Regs) -> Result<(), i32> {
    check(unsafe { open_tcb_write(tcb, resume as i32, regs as *const Regs as *const u32) })
}
pub fn tcb_read(tcb: u32) -> Result<Regs, i32> {
    let mut r = Regs::default();
    check(unsafe { open_tcb_read(tcb, &mut r as *mut Regs as *mut u32) })?;
    Ok(r)
}
pub fn tcb_set_tls(tcb: u32, base: u32) -> Result<(), i32> {
    check(unsafe { open_tcb_set_tls(tcb, base) })
}
pub fn tcb_suspend(tcb: u32) {
    unsafe { open_tcb_suspend(tcb) };
}
pub fn tcb_resume(tcb: u32) {
    unsafe { open_tcb_resume(tcb) };
}

pub const FAULT_UNKNOWN_SYSCALL: u32 = 2;
pub const FAULT_USER_EXCEPTION: u32 = 3;
pub const FAULT_VM: u32 = 5;
/// MR layout of an unknown-syscall fault: r0..r7, FaultIP, SP, LR, CPSR, number.
pub const MR_FAULT_IP: usize = 8;
pub const MR_SP: usize = 9;
pub const MR_LR: usize = 10;
pub const MR_CPSR: usize = 11;
pub const MR_SYSCALL: usize = 12;

pub struct Received {
    pub label: u32,
    pub length: u32,
    pub badge: u32,
    pub mrs: [u32; 16],
}
pub fn receive(block: bool) -> Received {
    let mut r = Received { label: 0, length: 0, badge: 0, mrs: [0; 16] };
    let info = unsafe { open_receive(SLOT_FAULT_EP, block as i32, &mut r.badge, r.mrs.as_mut_ptr()) };
    r.label = info >> 16;
    r.length = info & 0xffff;
    r
}
pub fn reply(mrs: &[u32]) {
    unsafe { open_reply(mrs.as_ptr(), mrs.len() as u32) }
}
pub fn send(cap: u32, mrs: &[u32]) {
    unsafe { open_send(cap, mrs.as_ptr(), mrs.len() as u32) }
}
pub fn yield_now() {
    unsafe { open_yield() }
}

/// The generic timer, which user mode may read directly.
pub fn counter() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("isb", "mrrc p15, 1, {lo}, {hi}, c14", lo = out(reg) lo, hi = out(reg) hi);
    }
    (u64::from(hi) << 32) | u64::from(lo)
}
pub fn counter_hz() -> u64 {
    let f: u32;
    unsafe { core::arch::asm!("mrc p15, 0, {f}, c14, c0, 0", f = out(reg) f) };
    u64::from(f.max(1))
}
pub fn now_ns() -> u64 {
    let c = counter();
    let hz = counter_hz();
    (c / hz) * 1_000_000_000 + (c % hz) * 1_000_000_000 / hz
}
