use crate::{power_backend, protocol::*};
pub fn run() -> ! {
    loop {
        let (caller,m) = Message::receive(SERVER);
        let reply = match (caller,m.key,m.op) {
            #[cfg(moto_board)]
            (21,0x4057,EXECUTE) => power_backend::usb_supply(),
            #[cfg(moto_board)]
            (21,0x4062,EXECUTE) if m.value <= 1 => power_backend::fastboot_mark(m.value != 0),
            #[cfg(moto_board)]
            (AUDIO_COMPONENT,PMIC_ACCESS,GET|EXECUTE) => power_backend::audio_pmic(m.op,m.value,m.arg),
            #[cfg(moto_board)]
            (CAMERA_COMPONENT,PMIC_ACCESS,GET|EXECUTE) => power_backend::camera_pmic(m.op,m.value,m.arg),
            (7,RADIO_RAILS,EXECUTE) if m.value <= 1 => power_backend::radio_rails(m.value != 0),
            (3,POWER,GET) => power_backend::read(),
            (3,POWER,EXECUTE) if m.value <= 2 => power_backend::command(m.value),
            _ => Message::error(DENIED),
        };
        reply.reply();
    }
}
