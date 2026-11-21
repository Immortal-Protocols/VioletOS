//! The Runtime process: connects the routing policy in router.rs to seL4 IPC,
//! the shared request cells and the supervisor health page.
use crate::protocol::*;
use crate::router::{Io, Out, Runtime};

struct Kernel;
impl Io for Kernel {
    fn queue(&mut self, lane: u32, index: u32, owner: u32, m: Message) -> bool {
        crate::lanes::queue(lane, index, owner, m)
    }
    fn take(&mut self, lane: u32, index: u32) -> Option<Out> {
        crate::lanes::take(lane, index).map(Out::unlabelled)
    }
    fn lifecycle(&mut self, m: Message) -> Message {
        m.call(crate::router::LIFECYCLE)
    }
    fn supervisor_status(&mut self) -> Message {
        unsafe {
            let cell = (0x07400000 as *const u32).add(980);
            let mut r = Message::result(cell.read_volatile());
            r.arg = cell.add(1).read_volatile();
            r.key = cell.add(2).read_volatile();
            r
        }
    }
    fn heartbeat(&mut self, who: u32, focus: u32) {
        unsafe {
            let health = 0x07400ff4 as *mut u32;
            let now = ticks();
            health.write_volatile(now);
            if health.add(1).read_volatile() != focus || who == focus {
                health.add(1).write_volatile(focus);
                health.add(2).write_volatile(now);
            }
        }
    }
    fn publish_status(&mut self, status: &Message) {
        // Publish boot diagnostics in the unused tail of the supervisor health page.
        // Only the supervisor copies this narrow record to the USB process.
        unsafe {
            let boot_status = 0x07400f80 as *mut u32;
            for (i, chunk) in status.text().chunks_exact(4).enumerate() {
                boot_status.add(i + 2).write_volatile(u32::from_le_bytes(chunk.try_into().unwrap()));
            }
            boot_status.add(1).write_volatile(status.arg);
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            boot_status.write_volatile(1);
        }
    }
    fn publish_presenter(&mut self, app: u32) {
        crate::lanes::publish_presenter(app);
    }
    fn reply(&mut self, out: Out) {
        out.m.reply();
    }
}

pub fn run() -> ! {
    let inventory = crate::lanes::initialize(9, LAUNCHER, Message::new(GET, INVENTORY, 0)).value;
    let mut io = Kernel;
    let mut rt = Runtime::start(inventory, &mut io);
    loop {
        let (who, m) = Message::receive(SERVER);
        rt.handle(who, m, &mut io);
    }
}
