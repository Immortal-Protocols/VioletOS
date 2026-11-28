//! The Linux ABI rules that join signal.rs, elf.rs, procmgr.rs and the
//! policy: what targets/qemu/test_abi.py's cfault, part of csig, cproc and
//! charden checked. Register state on delivery, the signal frame on a real
//! stack, VFP and Thumb state, SA_RESTART, timers firing, fork and execve
//! need the real kernel: targets/qemu/test_system.py still runs the C, Rust
//! and Go programs for those.
use crate::open::front::Touched;
use crate::open::signal::{self, SEGV_ACCERR, SEGV_MAPERR, SIGKILL, SIGSEGV, SIGSTOP};
use crate::open::vma::Access::{Read, Write};
use crate::world::*;

const CODE: u32 = 0x20d90;
const HANDLER: u32 = 0x21000;

fn phone() -> (World, usize) {
    let mut w = World::boot(Image::new().app("csig", "").app("cproc", ""));
    let i = w.open_app("csig");
    (w, i)
}

#[test]
fn a_fault_with_no_handler_ends_the_program_with_status_139() {
    // cfault
    let (mut w, i) = phone();
    assert_eq!(w.app(i).touch(0x1000, Read), Touched::Ended(139));
    assert_eq!(w.logged("csig (pid 101) exited with status 139"), 1);
    assert!(w.running(w.rt.home.unwrap()));
}

#[test]
fn sigsegv_reaches_a_handler_with_maperr_or_accerr() {
    // csig: SIGSEGV caught for unmapped and for read-only memory.
    let (mut w, i) = phone();
    let mut p = w.app(i);
    assert_eq!(p.sigaction(SIGSEGV, HANDLER, 0), Ok(0));
    assert_eq!(p.touch(0x1000, Read), Touched::Signal(SIGSEGV, SEGV_MAPERR));
    assert_eq!(p.touch(CODE, Write), Touched::Signal(SIGSEGV, SEGV_ACCERR));
    assert_eq!(p.delivered(), [(SIGSEGV, HANDLER), (SIGSEGV, HANDLER)]);
    assert!(w.running(i));
    assert_eq!(w.logged("bad memory access"), 0);
}

#[test]
fn a_fault_whose_signal_the_thread_blocks_ends_the_app_despite_a_handler() {
    // syscall.rs fault_signal: a blocked SIGSEGV is not delivered; the app ends.
    let (mut w, i) = phone();
    assert_eq!(w.app(i).sigaction(SIGSEGV, HANDLER, 0), Ok(0));
    w.rt.apps[i].as_mut().unwrap().thread.blocked |= signal::bit(SIGSEGV);
    assert_eq!(w.app(i).touch(0x1000, Read), Touched::Ended(139));
    assert!(!w.running(i));
}

#[test]
fn a_handler_that_never_fixes_the_fault_is_stopped() {
    // signal.rs FAULT_LIMIT: faults delivered with no system call between.
    let (mut w, i) = phone();
    assert_eq!(w.app(i).sigaction(SIGSEGV, HANDLER, 0), Ok(0));
    for _ in 0..signal::FAULT_LIMIT {
        assert_eq!(w.app(i).touch(0x1000, Read), Touched::Signal(SIGSEGV, SEGV_MAPERR));
    }
    assert_eq!(w.app(i).touch(0x1000, Read), Touched::Ended(139));
    // A program that makes calls between faults (a garbage collector's
    // write barrier, Go's nil checks) is never stopped for it.
    let j = w.open_app("csig");
    assert_eq!(w.app(j).sigaction(SIGSEGV, HANDLER, 0), Ok(0));
    for _ in 0..3 * signal::FAULT_LIMIT {
        assert_eq!(w.app(j).touch(0x1000, Read), Touched::Signal(SIGSEGV, SEGV_MAPERR));
        w.app(j).syscall(20, [0; 6]);
    }
    assert!(w.running(j));
}

#[test]
fn sigkill_and_sigstop_cannot_be_caught() {
    let (mut w, i) = phone();
    assert_eq!(w.app(i).sigaction(SIGKILL, HANDLER, 0), Err(EINVAL));
    assert_eq!(w.app(i).sigaction(SIGSTOP, HANDLER, 0), Err(EINVAL));
    assert_eq!(w.app(i).sigaction(65, HANDLER, 0), Err(EINVAL));
    assert_eq!(w.app(i).sigaction(15, HANDLER, 0), Ok(0));
}

#[test]
fn an_app_signals_itself_and_its_descendants_only() {
    // cproc: the kill rule, a child killed by a signal and its wait status.
    let (mut w, parent) = phone();
    let other = w.open_app("cproc");
    let home = w.rt.home.unwrap();
    let (other_pid, home_pid) = (w.app(other).pid(), w.app(home).pid());
    w.front(Some(parent));
    let child = w.fork(parent);
    let grandchild = w.fork(child);
    let (child_pid, grandchild_pid, parent_pid) = (w.app(child).pid(), w.app(grandchild).pid(), w.app(parent).pid());
    // Another app, Home, a pid nobody has, its own parent: refused.
    assert_eq!(w.app(parent).kill(other_pid as i32, 15), Err(EPERM));
    assert_eq!(w.app(parent).kill(home_pid as i32, SIGKILL), Err(EPERM));
    assert_eq!(w.app(parent).kill(9999, 15), Err(ESRCH));
    assert_eq!(w.app(child).kill(parent_pid as i32, 15), Err(EPERM));
    assert_eq!(w.app(other).kill(-1, SIGKILL), Err(ESRCH), "kill(-1) reaches only its own descendants: none");
    assert!(w.running(other) && w.running(home) && w.running(parent));
    // Signal 0 only asks.
    assert_eq!(w.app(parent).kill(grandchild_pid as i32, 0), Ok(0));
    // A handled signal runs the child's handler; a fatal one ends it and the
    // parent finds the wait status.
    assert_eq!(w.app(child).sigaction(10, HANDLER, 0), Ok(0));
    assert_eq!(w.app(parent).kill(child_pid as i32, 10), Ok(0));
    assert_eq!(w.app(child).delivered(), [(10, HANDLER)]);
    assert_eq!(w.app(child).kill(grandchild_pid as i32, 15), Ok(0));
    assert!(!w.running(grandchild));
    let zombies = w.rt.apps[child].as_ref().unwrap().sig.children.clone();
    assert_eq!(zombies.len(), 1);
    assert_eq!(signal::child_code(zombies[0].status.unwrap()), (signal::CLD_KILLED, 15));
    assert_eq!(signal::wait_pick(&zombies, -1, child_pid), Ok(Some(0)));
    assert_eq!(signal::wait_pick(&[], -1, child_pid), Err(()), "ECHILD");
    // SIGKILL ends a child whatever it handles.
    assert_eq!(w.app(child).sigaction(15, HANDLER, 0), Ok(0));
    assert_eq!(w.app(parent).kill(child_pid as i32, SIGKILL), Ok(0));
    assert!(!w.running(child));
    assert_eq!(w.logged(&format!("csig (pid {child_pid}) killed by signal 9")), 1);
}

#[test]
fn an_image_the_loader_refuses_does_not_start_and_the_caller_lives() {
    // charden: execve of a refused image fails with the caller alive.
    let rwx = elf(2, 0x20000, &[(0x20000, vec![0x22; 0x100], 0x100, 7)]);
    let mut w = World::boot(Image::new().app("charden", "").file("/apps/bad", &rwx).file("/apps/notelf", b"plain text"));
    let i = w.open_app("charden");
    let apps = w.rt.apps.iter().flatten().count();
    let free = w.rt.pool.available();
    for bad in ["/apps/bad", "/apps/notelf", "/apps/missing"] {
        assert!(w.start(bad, Some(i)).is_err(), "{bad}");
    }
    assert!(w.running(i));
    assert_eq!(w.rt.apps.iter().flatten().count(), apps);
    assert_eq!(w.rt.pool.available(), free, "a refused program leaves no pages behind");
}

#[test]
fn limits_from_signal_rs_hold_where_the_runtime_applies_them() {
    // charden: a 1 us timer fires at most once a millisecond; 64 zombies at
    // most; a "#!" line that names nothing is refused.
    assert_eq!(signal::timer_ns(1_000), signal::TIMER_MIN);
    assert_eq!(signal::timer_ns(5_000_000), 5_000_000);
    let kids: Vec<signal::Child> = (0..signal::MAX_CHILDREN as u32).map(|pid| signal::Child { pid, status: Some(0), exit_signal: 17 }).collect();
    assert!(!signal::room_for_child(&kids) && signal::room_for_child(&kids[1..]));
    assert_eq!(signal::shebang(b"#!/bin/python3 -u\nprint(1)\n"), Some(Ok(("/bin/python3", Some("-u")))));
    assert_eq!(signal::shebang(b"#! \n"), Some(Err(())));
    // An unaligned LDRD is SIGBUS BUS_ADRALN: the fault status says so.
    assert!(signal::alignment_fault(0x001) && !signal::alignment_fault(0x805));
}

#[test]
fn a_hidden_interpreter_answers_like_a_missing_one() {
    // charden: a script whose "#!" names a file the app may not see is
    // ENOENT, the same as one that does not exist (manifest.rs path_allowed
    // on the interpreter, syscall.rs execve).
    let mut w = World::boot(Image::new().app("charden", "").app("victim", ""));
    let victim = w.open_app("victim");
    w.app(victim).write_file("/data/victim", b"\x7fELF").unwrap();
    let i = w.open_app("charden");
    for (script, interp) in [(&b"#!/data/victim\n"[..], "/data/victim"), (&b"#!/data/nothing\n"[..], "/data/nothing")] {
        let (interpreter, _) = signal::shebang(script).unwrap().unwrap();
        assert_eq!(interpreter, interp);
        assert_eq!(w.rt.path_allowed(i, interpreter, false), Err(ENOENT), "{interp}");
    }
}
