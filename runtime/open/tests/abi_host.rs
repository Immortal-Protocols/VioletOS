//! Host tests for the Linux ABI pieces of the Open Runtime that have no
//! kernel calls: signal.rs (dispositions, masks, queues, the rt_sigframe
//! layout, the signal gate's offsets, timers, wait statuses, the kill rule,
//! "#!" lines) and elf.rs (the loader's header checks, PT_INTERP). Run by
//! `make test-unit`:
//!     rustc --edition=2021 --test -O runtime/open/tests/abi_host.rs -o build/tests/abi/host
#![allow(dead_code)]
extern crate alloc;

#[path = "../signal.rs"]
mod signal;
#[path = "../elf.rs"]
mod elf;
#[path = "../vma.rs"]
mod vma;

use signal::*;

fn word(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[test]
fn dispositions_follow_linux() {
    let dfl = Action::default();
    let ign = Action { handler: SIG_IGN, ..dfl };
    let caught = Action { handler: 0x1000, ..dfl };
    assert_eq!(disposition(&dfl, SIGCHLD), Disposition::Ignore);
    assert_eq!(disposition(&dfl, 23), Disposition::Ignore); // SIGURG
    assert_eq!(disposition(&dfl, 28), Disposition::Ignore); // SIGWINCH
    assert_eq!(disposition(&dfl, SIGSEGV), Disposition::Terminate { core: true });
    assert_eq!(disposition(&dfl, 15), Disposition::Terminate { core: false });
    assert_eq!(disposition(&dfl, 34), Disposition::Terminate { core: false }); // real-time
    assert_eq!(disposition(&dfl, 20), Disposition::Stop);
    assert_eq!(disposition(&ign, 15), Disposition::Ignore);
    assert_eq!(disposition(&caught, 10), Disposition::Handle);
    // SIGKILL and SIGSTOP cannot be caught or ignored, whatever the table says.
    assert_eq!(disposition(&caught, SIGKILL), Disposition::Terminate { core: false });
    assert_eq!(disposition(&ign, SIGSTOP), Disposition::Stop);
}

#[test]
fn masks_never_hold_kill_or_stop() {
    assert_eq!(bit(1), 1);
    assert_eq!(bit(64), 1 << 63);
    assert_eq!(bit(0), 0);
    assert_eq!(bit(65), 0);
    let all = u64::MAX;
    assert_eq!(mask_change(0, 0, all).unwrap() & UNBLOCKABLE, 0);
    assert_eq!(mask_change(all & !UNBLOCKABLE, 1, bit(10)).unwrap(), all & !UNBLOCKABLE & !bit(10));
    assert_eq!(mask_change(bit(3), 2, bit(5)).unwrap(), bit(5));
    assert_eq!(mask_change(0, 3, 0), None);
    let a = Action { handler: 0x1000, mask: bit(12) | bit(SIGKILL), ..Default::default() };
    assert_eq!(handler_mask(bit(2), &a, 10), bit(2) | bit(12) | bit(10));
    let nodefer = Action { flags: SA_NODEFER, ..a };
    assert_eq!(handler_mask(0, &nodefer, 10), bit(12));
    let t: Thread<()> = Thread::new(u64::MAX);
    assert_eq!(t.blocked & UNBLOCKABLE, 0);
}

#[test]
fn queues_hold_standard_signals_once_and_faults_first() {
    let mut q = Vec::new();
    let i = |signo| Info { signo, ..Default::default() };
    assert!(queue(&mut q, i(10)));
    assert!(!queue(&mut q, i(10)), "a standard signal is pending at most once");
    assert!(queue(&mut q, i(40)));
    assert!(queue(&mut q, i(40)), "real-time signals queue");
    assert!(queue(&mut q, i(2)));
    assert!(queue(&mut q, i(SIGSEGV)));
    assert!(!queue(&mut q, i(0)) && !queue(&mut q, i(65)));
    assert_eq!(set_of(&q), bit(10) | bit(40) | bit(2) | bit(SIGSEGV));
    // A fault goes before lower numbers; then the lowest; blocked ones wait.
    assert_eq!(take(&mut q, u64::MAX).unwrap().signo, SIGSEGV);
    assert_eq!(take(&mut q, !bit(2)).unwrap().signo, 10);
    assert_eq!(take(&mut q, u64::MAX).unwrap().signo, 2);
    assert_eq!(take(&mut q, u64::MAX).unwrap().signo, 40);
    assert_eq!(take(&mut q, u64::MAX).unwrap().signo, 40);
    assert_eq!(take(&mut q, u64::MAX), None);
    for _ in 0..100 {
        queue(&mut q, i(50));
    }
    assert_eq!(q.len(), 64, "the queue is bounded");
}

#[test]
fn ignoring_a_signal_discards_it() {
    let mut p: Proc<u32> = Proc::new(1);
    queue(&mut p.pending, Info { signo: 10, ..Default::default() });
    p.set_action(10, Action { handler: 0x1000, ..Default::default() });
    assert_eq!(p.pending.len(), 1);
    p.set_action(10, Action { handler: SIG_IGN, ..Default::default() });
    assert!(p.pending.is_empty());
    p.set_action(12, Action { handler: 0x2000, mask: u64::MAX, ..Default::default() });
    assert_eq!(p.action(12).mask & UNBLOCKABLE, 0);
    assert_eq!(p.action(0), Action::default());
    assert_eq!(p.action(99), Action::default());
}

#[test]
fn exec_and_fork_keep_what_linux_keeps() {
    let mut p: Proc<u32> = Proc::new(7);
    p.set_action(10, Action { handler: 0x1000, flags: SA_RESTART, ..Default::default() });
    p.set_action(13, Action { handler: SIG_IGN, ..Default::default() });
    p.timers.push(Timer { id: 0, at: 5, ..Default::default() });
    p.timers.push(Timer { id: ITIMER, at: 9, ..Default::default() });
    p.cloexec.push(3);
    p.children.push(Child { pid: 200, status: None, exit_signal: SIGCHLD });
    queue(&mut p.pending, Info { signo: 12, ..Default::default() });
    let child = p.fork_copy(100);
    assert_eq!(child.ppid, 100);
    assert_eq!(child.action(10).handler, 0x1000);
    assert!(child.pending.is_empty() && child.children.is_empty() && child.timers.is_empty());
    assert_eq!(child.cloexec, vec![3]);
    p.exec_reset();
    assert_eq!(p.action(10), Action::default(), "caught signals go back to the default");
    assert_eq!(p.action(13).handler, SIG_IGN, "ignored ones stay ignored");
    assert_eq!(p.timers.len(), 1);
    assert_eq!(p.timers[0].id, ITIMER, "the interval timer survives exec");
    assert_eq!(p.pending.len(), 1);
    assert_eq!(p.children.len(), 1);
    assert!(p.cloexec.is_empty());
}

#[test]
fn frame_layout_matches_linux_arm() {
    // Offsets from arch/arm/include/asm/ucontext.h and uapi/asm/sigcontext.h:
    // rt_sigframe { siginfo (128); ucontext { uc_flags, uc_link, uc_stack (12),
    // sigcontext (21 words), uc_sigmask (8 + 120 unused), uc_regspace[128] } (744); retcode[4] }.
    assert_eq!(FRAME, 128 + 744 + 16);
    assert_eq!(MCONTEXT, 128 + 20);
    assert_eq!(R0, MCONTEXT + 12);
    assert_eq!(SIGMASK, 128 + 20 + 21 * 4);
    assert_eq!(VFP, 128 + 232);
    assert_eq!(VFP_SIZE, 4 + 4 + 264 + 12 + 4); // vfp_sigframe, aligned to 8
    let ctx = Context { r: core::array::from_fn(|k| 0x100 + k as u32), sp: 0xbeef_0000, lr: 0x1234, pc: 0x5678, cpsr: 0x6000_0010 };
    let info = Info { signo: 11, code: SEGV_MAPERR, a: 0xdead_0000, b: 0, c: 0 };
    let alt = AltStack { sp: 0x7000_0000, size: 0x10000, flags: 0 };
    let mask = bit(10) | bit(40);
    let trap = Trap { no: 14, error: 0x805, addr: 0xdead_0000 };
    let f = encode(&ctx, &info, mask, &alt, &trap);
    assert_eq!(f.len(), FRAME as usize);
    assert_eq!((word(&f, 0), word(&f, 8), word(&f, 12)), (11, SEGV_MAPERR as u32, 0xdead_0000));
    assert_eq!((word(&f, 136), word(&f, 140), word(&f, 144)), (0x7000_0000, 0, 0x10000));
    assert_eq!((word(&f, 148), word(&f, 152), word(&f, 156)), (14, 0x805, mask as u32));
    assert_eq!(word(&f, 160), 0x100); // arm_r0
    assert_eq!(word(&f, 192), 0x108); // arm_r8, where the gate stores r8-r12
    assert_eq!(word(&f, 204), 0x10b); // arm_fp
    assert_eq!(word(&f, 212), 0xbeef_0000); // arm_sp
    assert_eq!(word(&f, 216), 0x1234); // arm_lr
    assert_eq!(word(&f, 220), 0x5678); // arm_pc
    assert_eq!(word(&f, 224), 0x6000_0010); // arm_cpsr
    assert_eq!(word(&f, 228), 0xdead_0000); // fault_address
    assert_eq!((word(&f, 232), word(&f, 236)), (mask as u32, (mask >> 32) as u32));
    assert_eq!((word(&f, 360), word(&f, 364)), (VFP_MAGIC, VFP_SIZE));
    assert_eq!(word(&f, 360 + VFP_SIZE as usize), 0, "the register space ends with a zero magic");
    let (back, m, a) = decode(&f[UC..]).unwrap();
    assert_eq!(back, ctx);
    assert_eq!(m, mask);
    assert_eq!(a, alt);
    // A handler cannot unblock SIGKILL through the saved mask either.
    let mut edited = f.clone();
    edited[SIGMASK..SIGMASK + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(decode(&edited[UC..]).unwrap().1, !UNBLOCKABLE);
    assert!(decode(&f[UC..UC + 50]).is_none());
}

/// Decode an ARM data-processing immediate ("add r4, sp, #imm").
fn arm_imm(w: u32) -> u32 {
    (w & 0xff).rotate_right(2 * ((w >> 8) & 0xf))
}

#[test]
fn gate_offsets_match_the_frame() {
    assert_eq!(GATE[0] & 0xffff_f000, 0xe28d_4000); // add r4, sp, #imm
    assert_eq!(arm_imm(GATE[0]) as usize, R8);
    assert_eq!(arm_imm(GATE[13]) as usize, R8);
    assert_eq!(arm_imm(GATE[2]) as usize, VFP + 8, "d0 is after the magic and size words");
    assert_eq!(arm_imm(GATE[8]) as usize, VFP + 8);
    assert_eq!(GATE[1], 0xe884_1f00); // stm r4, {r8-r12}
    assert_eq!(GATE[14], 0xe894_1f00); // ldm r4, {r8-r12}
    // vstmia/vldmia r4! of 16 doubles each: 32 doubles, then fpscr at VFP + 8 + 256.
    assert_eq!(GATE[3] & 0xff, 32);
    assert_eq!(GATE[4] & 0xff, 32);
    assert_eq!(GATE[7], 0xe12f_ff33); // blx r3
    assert_eq!(GATE[15], 0xe3a0_70ad); // mov r7, #173: rt_sigreturn
    assert_eq!(GATE[16], 0xef00_0000);
    assert!(GATE_OFFSET as usize + 4 * GATE.len() <= 0x10000);
    // The trampolines (8 bytes a thread) stop below the gate, with room for every thread.
    assert!(GATE_OFFSET / 8 >= 254);
    // The fpscr word sits inside the VFP block.
    assert!(8 + 256 + 4 <= VFP_SIZE as usize);
}

#[test]
fn frames_go_on_the_right_stack() {
    let alt = AltStack { sp: 0x7000_0000, size: 0x4000, flags: 0 };
    // No SA_ONSTACK: below sp, 8-byte aligned.
    assert_eq!(place(0xbe00_0104, &alt, false), Some((0xbe00_0104 - FRAME) & !7));
    // SA_ONSTACK: at the top of the alternate stack.
    let f = place(0xbe00_0000, &alt, true).unwrap();
    assert_eq!(f, (0x7000_4000 - FRAME) & !7);
    assert!(alt.on(f));
    // Already on it (a nested signal): below the current sp.
    assert_eq!(place(0x7000_2000, &alt, true), Some((0x7000_2000 - FRAME) & !7));
    // Overflowing the alternate stack is refused.
    assert_eq!(place(0x7000_0100, &alt, true), None);
    let tiny = AltStack { sp: 0x7000_0000, size: 256, flags: 0 };
    assert_eq!(place(0xbe00_0000, &tiny, true), None);
    // Disabled: the normal stack.
    let off = AltStack::default();
    assert_eq!(place(0xbe00_0000, &off, true), Some((0xbe00_0000 - FRAME) & !7));
    assert_eq!(place(100, &off, false), None, "no wrap below zero");
    assert_eq!(off.report(0), SS_DISABLE);
    assert_eq!(alt.report(0x7000_1000), SS_ONSTACK);
    assert_eq!(alt.report(0xbe00_0000), 0);
}

#[test]
fn async_delivery_waits_for_it_and_ge_state() {
    assert!(async_ok(0x6000_0010)); // ARM, flags only
    assert!(async_ok(0x0000_0030)); // Thumb, outside an IT block
    assert!(!async_ok(0x0000_0430)); // Thumb inside an IT block (IT[2] set)
    assert!(!async_ok(0x0200_0030)); // IT[0]
    assert!(!async_ok(0x0001_0010)); // GE set (lost by a register write)
    assert_eq!(user_cpsr(0xf800_00ff), 0xf800_0010);
    assert!(thumb(0x30) && !thumb(0x10));
}

#[test]
fn restart_rules() {
    assert!(restarts(3, SA_RESTART)); // read
    assert!(restarts(114, SA_RESTART)); // wait4
    assert!(restarts(240, SA_RESTART)); // futex
    assert!(!restarts(3, 0), "without SA_RESTART: EINTR");
    for never in [168, 336, 414, 142, 252, 346, 162, 265, 407, 179, 29, 177, 283] {
        assert!(!restarts(never, SA_RESTART), "{never} never restarts");
    }
}

#[test]
fn timers_fire_and_count_overruns() {
    let mut t = Timer { id: 0, at: 1000, every: 0, sig: SIGALRM, ..Default::default() };
    assert!(!t.expire(999));
    assert_eq!(t.left(400), 600);
    assert!(t.expire(1000));
    assert_eq!(t.at, 0, "a one-shot timer disarms");
    assert!(!t.expire(5000));
    assert_eq!(t.left(5000), 0);
    let ms = TIMER_MIN;
    let mut p = Timer { id: 1, at: 10 * ms, every: ms, ..Default::default() };
    assert!(p.expire(10 * ms + 50));
    assert_eq!((p.at, p.overrun), (11 * ms, 0));
    assert!(p.expire(14 * ms + 200));
    assert_eq!((p.at, p.overrun), (15 * ms, 3));
    assert_eq!(soonest(&[t, p], 0), Some(15 * ms));
    assert_eq!(soonest(&[t], 0), None);
    assert_eq!(free_id(&[p]), Some(0));
    let full: Vec<Timer> = (0..32).map(|id| Timer { id, ..Default::default() }).collect();
    assert_eq!(free_id(&full), None);
}

#[test]
fn wait_statuses_and_matching() {
    assert_eq!(exited(7), 0x700);
    assert_eq!(exited(-1), 0xff00);
    assert_eq!(signaled(SIGSEGV, true), 0x8b);
    assert_eq!(signaled(9, false), 9);
    assert_eq!(child_code(exited(3)), (CLD_EXITED, 3));
    assert_eq!(child_code(signaled(9, false)), (CLD_KILLED, 9));
    assert_eq!(child_code(signaled(11, true)), (CLD_DUMPED, 11));
    let kids = [Child { pid: 201, status: None, exit_signal: SIGCHLD }, Child { pid: 202, status: Some(0x100), exit_signal: SIGCHLD }];
    assert_eq!(wait_pick(&kids, -1, 100), Ok(Some(1)));
    assert_eq!(wait_pick(&kids, 201, 100), Ok(None));
    assert_eq!(wait_pick(&kids, 202, 100), Ok(Some(1)));
    assert_eq!(wait_pick(&kids, 0, 100), Ok(Some(1)));
    assert_eq!(wait_pick(&kids, -100, 100), Ok(Some(1)));
    assert_eq!(wait_pick(&kids, 300, 100), Err(()));
    assert_eq!(wait_pick(&kids, -55, 100), Err(()));
    assert_eq!(wait_pick(&[], -1, 100), Err(()));
}

#[test]
fn apps_signal_only_themselves_and_their_descendants() {
    assert!(may_signal(100, 100, &[]));
    assert!(may_signal(100, 300, &[200, 100]));
    assert!(!may_signal(100, 300, &[200, 1]));
    assert!(!may_signal(300, 100, &[1]), "a child may not signal its parent");
    assert!(!may_signal(100, 101, &[1]), "nor one app another");
}

#[test]
fn shebang_lines() {
    assert_eq!(shebang(b"\x7fELF"), None);
    assert_eq!(shebang(b"#!/bin/python3\nprint(1)"), Some(Ok(("/bin/python3", None))));
    assert_eq!(shebang(b"#! /bin/sh -e -x\n"), Some(Ok(("/bin/sh", Some("-e -x")))));
    assert_eq!(shebang(b"#!/bin/sh"), Some(Ok(("/bin/sh", None))));
    assert_eq!(shebang(b"#!\n"), Some(Err(())));
    let mut long = b"#!/".to_vec();
    long.extend(std::iter::repeat(b'a').take(300));
    assert_eq!(shebang(&long), Some(Err(())));
}

// ---- elf.rs ----

/// A minimal ELF32 ARM file: the header, program headers, then `tail`.
fn elf_file(kind: u16, phdrs: &[[u32; 8]], tail: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 52];
    b[..6].copy_from_slice(b"\x7fELF\x01\x01");
    b[16..18].copy_from_slice(&kind.to_le_bytes());
    b[18..20].copy_from_slice(&40u16.to_le_bytes());
    b[24..28].copy_from_slice(&0x1234u32.to_le_bytes());
    b[28..32].copy_from_slice(&52u32.to_le_bytes());
    b[42..44].copy_from_slice(&32u16.to_le_bytes());
    b[44..46].copy_from_slice(&(phdrs.len() as u16).to_le_bytes());
    for h in phdrs {
        for w in h {
            b.extend_from_slice(&w.to_le_bytes());
        }
    }
    b.extend_from_slice(tail);
    b
}
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;

#[test]
fn static_programs_load_as_before() {
    let text = [PT_LOAD, 0, 0x10000, 0, 0x200, 0x200, 5, 0x10000];
    let data = [PT_LOAD, 0x100, 0x30100, 0, 0x40, 0x2000, 6, 0x10000];
    let f = elf_file(2, &[text, data], &[0u8; 0x200]);
    let p = elf::plan(&f).unwrap();
    assert!(!p.pie && !p.interp);
    assert_eq!(p.entry, 0x1234);
    assert_eq!(p.segs.len(), 2);
    assert_eq!(p.span(), (0x10000, 0x33000));
    assert_eq!(p.phdr_at(0), 0x10000 + 52, "headers found in the first segment");
    let pie = elf_file(3, &[[PT_PHDR, 52, 52, 0, 64, 64, 4, 4], text], &[0u8; 0x200]);
    let p = elf::plan(&pie).unwrap();
    assert!(p.pie);
    assert_eq!(p.phdr_at(0x40_0000), 0x40_0000 + 52);
    assert!(!p.loader_ok() || p.pie);
}

#[test]
fn bad_headers_are_refused() {
    let text = [PT_LOAD, 0, 0x10000, 0, 0x200, 0x200, 5, 0x10000];
    let ok = elf_file(2, &[text], &[0u8; 0x200]);
    assert!(elf::plan(&ok).is_ok());
    assert!(elf::plan(&ok[..40]).is_err());
    let mut bad = ok.clone();
    bad[18] = 3; // x86
    assert_eq!(elf::plan(&bad).unwrap_err(), "not an ARM executable");
    let mut bad = ok.clone();
    bad[16] = 1; // ET_REL
    assert_eq!(elf::plan(&bad).unwrap_err(), "not an executable");
    let mut bad = ok.clone();
    bad[44] = 200; // more headers than the file holds
    assert_eq!(elf::plan(&bad).unwrap_err(), "bad program headers");
    let wx = elf_file(2, &[[PT_LOAD, 0, 0x10000, 0, 0x200, 0x200, 7, 0x10000]], &[0u8; 0x200]);
    assert_eq!(elf::plan(&wx).unwrap_err(), "a segment is writable and executable");
    let past = elf_file(2, &[[PT_LOAD, 0x100, 0x10000, 0, 0xffff_ff80, 0xffff_ff80, 5, 0]], &[]);
    assert_eq!(elf::plan(&past).unwrap_err(), "bad segment", "offset + filesz overflows");
    let big = elf_file(2, &[[PT_LOAD, 0, 0x10000, 0, 0x300, 0x200, 5, 0]], &[0u8; 0x400]);
    assert_eq!(elf::plan(&big).unwrap_err(), "bad segment", "filesz > memsz");
    let wrap = elf_file(2, &[[PT_LOAD, 0, 0xffff_f000, 0, 0x10, 0x2000, 6, 0]], &[0u8; 0x10]);
    assert_eq!(elf::plan(&wrap).unwrap_err(), "segment too high");
    let none = elf_file(2, &[[PT_PHDR, 52, 52, 0, 32, 32, 4, 4]], &[]);
    assert_eq!(elf::plan(&none).unwrap_err(), "no loadable segments");
}

#[test]
fn only_musls_loader_may_be_the_interpreter() {
    let text = [PT_LOAD, 0, 0x10000, 0, 0x200, 0x200, 5, 0x10000];
    let at = 52 + 2 * 32;
    let mut path = elf::LOADER.as_bytes().to_vec();
    path.push(0);
    let interp = [PT_INTERP, at, 0, 0, path.len() as u32, path.len() as u32, 4, 1];
    let mut tail = path.clone();
    tail.resize(0x200, 0);
    let dynamic = elf_file(3, &[interp, text], &tail);
    let p = elf::plan(&dynamic).unwrap();
    assert!(p.interp && p.pie);
    assert!(!p.loader_ok(), "a program with an interpreter is not one");
    let refused = |name: &[u8]| {
        let mut t = name.to_vec();
        let n = t.len() as u32;
        t.resize(0x200, 0);
        elf::plan(&elf_file(3, &[[PT_INTERP, at, 0, 0, n, n, 4, 1], text], &t)).is_err()
    };
    assert!(refused(b"/lib/ld-linux-armhf.so.3\0"), "glibc's loader");
    assert!(refused(b"/system/bin/linker\0"), "Android's linker");
    assert!(refused(b"/lib/ld-musl-armhf.so.1"), "not NUL-terminated");
    assert!(refused(b"/lib/ld-musl-armhf.so.1x\0"), "a longer name");
    assert!(refused(b"/tmp/ld-musl-armhf.so.1\0"), "an app-written copy elsewhere");
    assert!(refused(b"lib/ld-musl-armhf.so.1\0"), "a relative path");
    // Pointing outside the file, or twice.
    assert!(elf::plan(&elf_file(3, &[[PT_INTERP, 0xffff_fff0, 0, 0, 24, 24, 4, 1], text], &tail)).is_err());
    assert!(elf::plan(&elf_file(3, &[interp, interp, text], &tail)).is_err());
}

#[test]
fn random_bytes_never_panic() {
    // The loader parses app files inside the runtime: no input may crash it.
    let mut seed = 0x9e37_79b9u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let text = [PT_LOAD, 0, 0x10000, 0, 0x200, 0x200, 5, 0x10000];
    let mut tail = elf::LOADER.as_bytes().to_vec();
    tail.push(0);
    tail.resize(0x200, 0);
    let base = elf_file(3, &[[PT_INTERP, 116, 0, 0, 24, 24, 4, 1], text], &tail);
    assert!(elf::plan(&base).is_ok_and(|p| p.interp));
    for _ in 0..20000 {
        let mut f = base.clone();
        for _ in 0..1 + next() % 8 {
            let at = next() as usize % f.len();
            f[at] = next() as u8;
        }
        let len = next() as usize % (f.len() + 1);
        let _ = elf::plan(&f[..len]);
        let _ = elf::plan(&f);
        if let Ok(p) = elf::plan(&f) {
            let _ = (p.span(), p.phdr_at(next()));
        }
    }
}

/// A dynamically linked program that names musl's loader.
fn dynamic_program() -> Vec<u8> {
    let text = [PT_LOAD, 0, 0x10000, 0, 0x200, 0x200, 5, 0x10000];
    let mut path = elf::LOADER.as_bytes().to_vec();
    path.push(0);
    let interp = [PT_INTERP, 52 + 2 * 32, 0, 0, path.len() as u32, path.len() as u32, 4, 1];
    path.resize(0x200, 0);
    elf_file(3, &[interp, text], &path)
}

/// musl's libc.so (the loader) as make test-abi builds it: the shipped
/// dry run (elf::check, what execve and the loader use) must take it.
/// make test-abi sets VIOLET_ABI_REQUIRE_LIBC=1 so a missing build fails.
#[test]
fn the_shipped_loader_passes_the_wx_rules() {
    let candidates = ["build/open-toolchain/musl-shared/lib/libc.so", "build/tests/abi/musl-shared/lib/libc.so"];
    let Some(data) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
        assert!(std::env::var_os("VIOLET_ABI_REQUIRE_LIBC").is_none(), "musl's libc.so has not been built");
        eprintln!("skipping: musl's libc.so has not been built (make test-abi builds it)");
        return;
    };
    let p = elf::plan(&data).expect("the loader's headers");
    assert!(p.loader_ok());
    for seed in 0..64u8 {
        let layout = vma::Layout::random(&[seed; 24]);
        elf::check(&dynamic_program(), Some(&data), &layout).expect("libc.so as the loader");
    }
}

/// execve's dry run refuses, before the caller is torn down, every image
/// the loader would refuse after: code sharing a 64 KiB page with data
/// (which elf::plan alone lets through), a segment above the program area,
/// a missing or bad loader.
#[test]
fn the_dry_run_finds_what_the_loader_refuses() {
    let layout = vma::Layout::fixed();
    let text = [PT_LOAD, 0, 0x10000, 0, 0x200, 0x200, 5, 0x10000];
    let apart = [PT_LOAD, 0x100, 0x20000, 0, 0x40, 0x2000, 6, 0x10000];
    let ok = elf_file(2, &[text, apart], &[0u8; 0x200]);
    assert_eq!(elf::check(&ok, None, &layout), Ok(()));
    // Data in the same 64 KiB page as the code, not the same 4 KiB page.
    let near = [PT_LOAD, 0x100, 0x18100, 0, 0x40, 0x2000, 6, 0x10000];
    let mixed = elf_file(2, &[text, near], &[0u8; 0x200]);
    assert!(elf::plan(&mixed).is_ok(), "the headers alone are fine");
    assert_eq!(elf::check(&mixed, None, &layout), Err("code shares a 64 KiB page with data"));
    // Data in the code's own 4 KiB page.
    let same = [PT_LOAD, 0x100, 0x10100, 0, 0x40, 0x100, 6, 0x10000];
    assert!(elf::check(&elf_file(2, &[text, same], &[0u8; 0x200]), None, &layout).is_err());
    // Past the program area (the mmap area starts at MMAP_BASE).
    let high = [PT_LOAD, 0, vma::MMAP_BASE - 0x1000, 0, 0x200, 0x2000, 5, 0x10000];
    assert_eq!(elf::check(&elf_file(2, &[high], &[0u8; 0x200]), None, &layout), Err("segment too high"));
    // A dynamically linked program needs a loader that keeps the rules too.
    let prog = dynamic_program();
    assert!(elf::check(&prog, None, &layout).is_err());
    let ld_text = [PT_LOAD, 0, 0, 0, 0x200, 0x200, 5, 0x10000];
    let ld_data = [PT_LOAD, 0x100, 0x10000, 0, 0x40, 0x1000, 6, 0x10000];
    let ld = elf_file(3, &[ld_text, ld_data], &[0u8; 0x200]);
    assert_eq!(elf::check(&prog, Some(&ld), &layout), Ok(()));
    let ld_mixed = elf_file(3, &[ld_text, [PT_LOAD, 0x100, 0x8000, 0, 0x40, 0x1000, 6, 0x10000]], &[0u8; 0x200]);
    assert!(elf::check(&prog, Some(&ld_mixed), &layout).is_err());
    assert!(elf::check(&prog, Some(&prog), &layout).is_err(), "a loader with an interpreter");
}

/// A timer of a few microseconds must not keep the runtime awake: periods
/// and first expiries are at least TIMER_MIN, a timer that somehow has a
/// shorter one still moves on by TIMER_MIN, and the run loop is never asked
/// to look again sooner than TIMER_MIN from now.
#[test]
fn timers_have_a_floor() {
    assert_eq!(timer_ns(0), 0);
    assert_eq!(timer_ns(1), TIMER_MIN);
    assert_eq!(timer_ns(1000), TIMER_MIN);
    assert_eq!(timer_ns(5 * TIMER_MIN), 5 * TIMER_MIN);
    // A 1 us periodic timer (as setitimer(1us, 1us) asked before the clamp).
    let mut t = Timer { id: ITIMER, at: 100, every: 1000, sig: SIGALRM, ..Default::default() };
    let mut now = 100;
    let mut fired = 0;
    for _ in 0..1000 {
        if t.expire(now) {
            fired += 1;
        }
        assert!(t.at >= now + TIMER_MIN || t.at > now, "re-armed in the past");
        now += 1000; // the loop spinning every microsecond
    }
    // 1 ms of spinning: at most a couple of expiries, not a thousand.
    assert!(fired <= 2, "fired {fired} times in 1 ms");
    // Overdue timers do not make the loop spin either.
    let late = Timer { id: 0, at: 5, every: 0, ..Default::default() };
    assert_eq!(soonest(&[late], 1_000_000_000), Some(1_000_000_000 + TIMER_MIN));
}

/// A SIGSEGV handler that returns to the same fault: rt_sigreturn (which
/// ends every handler) must not reset the fault count, other calls do.
#[test]
fn sigreturn_does_not_reset_the_fault_count() {
    assert!(!fault_reset(173));
    assert!(fault_reset(4));
    assert!(fault_reset(162));
    let mut t: Thread<()> = Thread::new(0);
    let mut rounds = 0;
    // fault -> handler -> rt_sigreturn -> the same fault, as syscall() and fault_signal count them.
    while t.faults <= FAULT_LIMIT {
        t.faults += 1; // fault_signal
        if fault_reset(173) {
            t.faults = 0; // syscall() at the gate's rt_sigreturn
        }
        rounds += 1;
        assert!(rounds < 10 * FAULT_LIMIT, "the re-fault loop never ends");
    }
    assert_eq!(rounds, FAULT_LIMIT + 1);
}

#[test]
fn alignment_faults_are_told_apart() {
    // Short descriptors: FS[4] is bit 10, FS[3:0] bits 3:0; WnR (bit 11) does not matter.
    assert!(alignment_fault(0x001));
    assert!(alignment_fault(0x801));
    assert!(!alignment_fault(0x005)); // translation fault, section
    assert!(!alignment_fault(0x007)); // translation fault, page
    assert!(!alignment_fault(0x00f)); // permission fault
    assert!(!alignment_fault(0x401)); // FS 0b10001: not alignment
    // LPAE (bit 9): status 0b100001 is alignment.
    assert!(alignment_fault(0x221));
    assert!(!alignment_fault(0x207));
}

#[test]
fn zombies_and_children_are_capped() {
    let mut kids = Vec::new();
    while room_for_child(&kids) {
        kids.push(Child { pid: 100 + kids.len() as u32, status: Some(0), exit_signal: SIGCHLD });
        assert!(kids.len() <= 1000, "children are never capped");
    }
    assert_eq!(kids.len(), MAX_CHILDREN);
    // Reaping one makes room again.
    let k = wait_pick(&kids, -1, 1).unwrap().unwrap();
    kids.remove(k);
    assert!(room_for_child(&kids));
}

/// Violet Shield sees the program that runs, never argv[0].
#[test]
fn shield_sees_the_real_program() {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    assert_eq!(exec_line("/tmp/payload", &s(&["/apps/home"])), "/tmp/payload");
    assert_eq!(exec_line("/tmp/payload", &s(&["/apps/home", "-x"])), "/tmp/payload -x");
    // A script: execve has put the interpreter first and the script after it.
    assert_eq!(exec_line("/bin/python3", &s(&["/bin/python3", "/tmp/s.py", "a"])), "/bin/python3 /tmp/s.py a");
}

/// The fork helper: the gate calls it like a handler (blx r3), it makes the
/// private FORK_NR call and returns into the gate.
#[test]
fn the_fork_helper_follows_the_gate() {
    let k = ((FORK_HELPER - GATE_OFFSET) / 4) as usize;
    assert_eq!(GATE[k - 1], 0xe7f000f0, "the helper follows the gate's last word");
    let mov = GATE[k];
    assert_eq!(mov & 0xffff_f000, 0xe3a0_7000, "mov r7, #imm");
    let (rot, imm) = ((mov >> 8) & 0xf, mov & 0xff);
    assert_eq!(imm.rotate_right(2 * rot), FORK_NR);
    assert_eq!(GATE[k + 1], 0xef000000); // svc #0
    assert_eq!(GATE[k + 2], 0xe12fff1e); // bx lr
    assert!(GATE_OFFSET as usize + GATE.len() * 4 <= 0x10000, "the gate fits in the trampoline page");
}
