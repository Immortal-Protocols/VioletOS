//! The target backend owns these mappings for the lifetime of the touch task.
//! The QEMU backend implements the same register interface in a fixture.
extern "C" {
    fn spi_read(offset: u32) -> u32;
    fn spi_write(offset: u32, value: u32);
    fn spi_clock_start() -> bool;
    fn ticks() -> u32;
    static ticks_per_ms: u32;
    fn delay_ms(ms: u32);
    fn watchdog_progress();
}

fn read(offset: u32) -> u32 {
    unsafe { spi_read(offset) }
}
fn write(offset: u32, value: u32) {
    unsafe { spi_write(offset, value) }
}
pub fn wait(ms: u32) {
    unsafe { delay_ms(ms) }
}
pub fn progress() {
    unsafe { watchdog_progress() }
}
fn configure() {
    write(0x18, 4);
    read(0x18);
    write(0x18, 0);
    write(0x24, 0);
    write(0, 0x00070007);
    write(0x28, 0x00030003);
}

#[derive(Default)]
pub struct Bus {
    pub state: u32,
    pub command: u32,
}
impl Bus {
    pub fn start(&self) -> bool {
        if !unsafe { spi_clock_start() } {
            return false;
        }
        configure();
        true
    }
    pub fn exchange(&mut self, data: &mut [u8]) -> bool {
        if data.is_empty() || data.len() > 32 {
            return false;
        }
        read(0x1c);
        write(4, ((data.len() as u32 - 1) << 16) | 7);
        for chunk in data.chunks(4) {
            let mut bytes = [0; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            write(0x10, u32::from_le_bytes(bytes));
        }
        write(0x18, 0x13001);
        let start = unsafe { ticks() };
        let complete = || read(0x20) & 1 != 0 && read(0x1c) & 1 != 0;
        while !complete() {
            if unsafe { ticks() }.wrapping_sub(start) > unsafe { ticks_per_ms } * 10 {
                if complete() {
                    break;
                }
                self.state = read(0x20);
                self.command = read(0x18);
                configure();
                return false;
            }
        }
        for chunk in data.chunks_mut(4) {
            chunk.copy_from_slice(&read(0x14).to_le_bytes()[..chunk.len()]);
        }
        true
    }
}
