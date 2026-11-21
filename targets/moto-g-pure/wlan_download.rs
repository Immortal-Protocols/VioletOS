//! MT6765 Connac ROM download. The transport owns the command and data rings.
//! Returned frames have no bus padding; ring descriptors use the encoded length.
use super::wlan_image::{regions, Destination};

const HEADER: usize = 64;
const CHUNK: usize = 2048;

pub trait Transport {
    type Error;
    fn command(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
    // The transport strips the hardware RX descriptor. The event remains intact.
    fn command_result(&mut self) -> Result<&[u8], Self::Error>;
    fn data(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
}

#[derive(Debug, PartialEq)]
pub enum Error<E> {
    Image,
    Bus(E),
    BadAck,
    Rejected(u8),
}

fn packet<'a>(
    buf: &'a mut [u8; HEADER + CHUNK],
    cid: u8,
    seq: &mut u8,
    payload: &[u8],
) -> &'a [u8] {
    let size = HEADER + payload.len();
    buf[..HEADER].fill(0);
    buf[..2].copy_from_slice(&(size as u16).to_le_bytes());
    let (queue, format): (u16, u8) = if cid == 0 { (0xf800, 3) } else { (0x8000, 2) };
    buf[2..4].copy_from_slice(&queue.to_le_bytes());
    buf[5] = 0xa0;
    buf[7] = format;
    buf[32..34].copy_from_slice(&((size - 32) as u16).to_le_bytes());
    buf[36] = cid;
    buf[37] = 0xa0;
    *seq = seq.wrapping_add(1);
    buf[39] = *seq;
    buf[HEADER..size].copy_from_slice(payload);
    &buf[..size]
}

pub fn upload<T: Transport>(
    image: &[u8],
    seq: &mut u8,
    bus: &mut T,
) -> Result<(), Error<T::Error>> {
    let segments = regions(image).ok_or(Error::Image)?;
    let mut buf = [0; HEADER + CHUNK];
    for segment in segments {
        let Destination::Chip(address) = segment.destination else {
            continue;
        };
        // This factory image uses the same encryption and key flags on all
        // three chip-local sections. The ROM resets its cipher per section.
        if segment.flags != 7 {
            return Err(Error::Image);
        }
        let mut config = [0; 12];
        config[..4].copy_from_slice(&address.to_le_bytes());
        config[4..8].copy_from_slice(&(segment.bytes.len() as u32).to_le_bytes());
        config[8..].copy_from_slice(&0x8000000fu32.to_le_bytes());
        bus.command(packet(&mut buf, 1, seq, &config))
            .map_err(Error::Bus)?;
        let event = bus.command_result().map_err(Error::Bus)?;
        if event.len() < 12
            || usize::from(u16::from_le_bytes([event[0], event[1]])) != event.len()
            || event[2..4] != [0, 0xe0]
            || event[4] != 1
            || event[5] != *seq
        {
            return Err(Error::BadAck);
        }
        if event[8] != 0 {
            return Err(Error::Rejected(event[8]));
        }
        for chunk in segment.bytes.chunks(CHUNK) {
            bus.data(packet(&mut buf, 0, seq, chunk))
                .map_err(Error::Bus)?;
        }
    }
    Ok(())
}
