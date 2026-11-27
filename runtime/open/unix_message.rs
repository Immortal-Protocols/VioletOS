//! Linux ARM sendmsg/recvmsg on a local stream. A queued SCM_RIGHTS holds
//! open file descriptions until read, close, or successful delivery. Passing
//! a local socket is rejected: queued sockets can form cycles that Rc cannot collect.
use super::device::Kind;
use super::syscall::{Outcome, EAGAIN, EBADF, EFAULT, EINVAL};
use super::{FileRef, Runtime, Wait};
use alloc::rc::Rc;
use alloc::vec::Vec;

const FD_MAX: usize = 253; // Linux SCM_MAX_FD
const CMSG_HEADER: usize = 12;
const MSG_DONTWAIT: u32 = 0x40;
const MSG_CMSG_CLOEXEC: u32 = 0x40000000;
const MSG_CTRUNC: u32 = 8;
const EOPNOTSUPP: i32 = -95;

/// Prepared once before blocking. A concurrent close/reuse of a sending
/// descriptor cannot change which file is delivered when there is room.
pub enum Message {
    Send { socket: FileRef, data: Vec<u8>, files: Vec<FileRef> },
    Receive { socket: FileRef, iov: Vec<(u32, usize)>, bytes: usize, header: u32, control: u32, capacity: usize, cloexec: bool },
}

impl Runtime {
    fn rights_from(&mut self, i: usize, control: u32, size: usize) -> Result<Vec<FileRef>, i32> {
        let mut files = Vec::new();
        if size == 0 { return Ok(files); }
        if size > FD_MAX * (CMSG_HEADER + 4) { return Err(EINVAL); }
        let bytes = self.rd(i, control, size).ok_or(EFAULT)?;
        let mut pos = 0;
        while pos + CMSG_HEADER <= bytes.len() {
            let header = &bytes[pos..pos + CMSG_HEADER];
            let word = |at| u32::from_le_bytes(header[at..at+4].try_into().unwrap());
            let len = word(0) as usize;
            if len < CMSG_HEADER || len > bytes.len() - pos { return Err(EINVAL); }
            if word(4) != 1 || word(8) != 1 { return Err(EOPNOTSUPP); } // SOL_SOCKET, SCM_RIGHTS
            let data = &bytes[pos + CMSG_HEADER..pos + len];
            if data.len() % 4 != 0 || files.len() + data.len() / 4 > FD_MAX { return Err(EINVAL); }
            for raw in data.chunks_exact(4) {
                let fd = i32::from_le_bytes(raw.try_into().unwrap());
                let file = self.file(i, fd).ok_or(EBADF)?;
                if matches!(file.borrow().kind, Kind::Local(_)) { return Err(EOPNOTSUPP); }
                files.push(file);
            }
            pos += (len + 3) & !3;
        }
        Ok(files)
    }

    pub fn unix_message(&mut self, i: usize, fd: i32, socket: FileRef, header: u32, h: &[u32], flags: u32, send: bool) -> Outcome {
        // MSG_NOSIGNAL is immaterial until signal delivery exists.
        let allowed = MSG_DONTWAIT | if send { 0x4000 } else { MSG_CMSG_CLOEXEC };
        if flags & !allowed != 0 { return Outcome::Done(EOPNOTSUPP); }
        if h[3] > 1024 { return Outcome::Done(-90); } // EMSGSIZE
        let Some(raw) = self.rd_words(i, h[2], h[3] as usize * 2) else { return Outcome::Done(EFAULT) };
        let mut bytes = 0;
        let mut iov = Vec::new();
        for vec in raw.chunks_exact(2) {
            let n = (vec[1] as usize).min(65536 - bytes);
            if n > 0 { iov.push((vec[0], n)); }
            bytes += n;
            if bytes == 65536 { break; }
        }
        let nonblock = flags & MSG_DONTWAIT != 0 || socket.borrow().flags & 0o4000 != 0;
        let message = if send {
            let files = match self.rights_from(i, h[4], h[5] as usize) {
                Ok(f) => f,
                Err(e) => return Outcome::Done(e),
            };
            let mut data = Vec::with_capacity(bytes);
            for (base, n) in iov {
                let Some(part) = self.rd(i, base, n) else { return Outcome::Done(EFAULT) };
                data.extend(part);
            }
            Message::Send { socket, data, files }
        } else {
            Message::Receive { socket, iov, bytes, header, control: h[4], capacity: if h[4] == 0 { 0 } else { h[5] as usize },
                               cloexec: flags & MSG_CMSG_CLOEXEC != 0 }
        };
        let result = self.unix_message_now(i, &message);
        if result == EAGAIN && !nonblock {
            let deadline = if send { None } else { self.recv_deadline(i, fd) };
            return Outcome::Block(Wait::UnixMessage(Rc::new(message)), deadline);
        }
        Outcome::Done(result)
    }

    pub fn unix_message_now(&mut self, i: usize, message: &Message) -> i32 {
        match message {
            Message::Send { socket, data, files } => {
                let file = socket.borrow();
                let Kind::Local(stream) = &file.kind else { return EBADF };
                stream.send_files(data, files)
            }
            Message::Receive { socket, iov, bytes, header, control, capacity, cloexec } => {
                let file = socket.borrow();
                let Kind::Local(stream) = &file.kind else { return EBADF };
                let (data, files) = match stream.preview(*bytes) {
                    Ok(packet) => packet,
                    Err(e) => return e,
                };
                let mut at = 0;
                for &(base, size) in iov {
                    if at == data.len() { break; }
                    let end = (at + size).min(data.len());
                    if !self.wr(i, base, &data[at..end]) { return EFAULT; }
                    at = end;
                }
                let room = capacity.saturating_sub(CMSG_HEADER) / 4;
                let mut fds = Vec::new();
                for f in files.iter().take(room) {
                    let fd = self.install_ref(i, f.clone(), 0, *cloexec);
                    if fd < 0 { break; }
                    fds.push(fd);
                }
                let mut ancillary = Vec::new();
                if !fds.is_empty() {
                    let len = (CMSG_HEADER + 4 * fds.len()) as u32;
                    for word in [len, 1, 1] { ancillary.extend(word.to_le_bytes()); }
                    for fd in &fds { ancillary.extend(fd.to_le_bytes()); }
                }
                let flags = if fds.len() < files.len() { MSG_CTRUNC } else { 0 };
                let stored = self.wr(i, *control, &ancillary)
                    && self.wr(i, header + 4, &0u32.to_le_bytes())
                    && self.wr(i, header + 20, &(ancillary.len() as u32).to_le_bytes())
                    && self.wr(i, header + 24, &flags.to_le_bytes());
                if !stored {
                    let app = self.apps[i].as_mut().unwrap();
                    for fd in fds { app.fds[fd as usize] = None; }
                    return EFAULT;
                }
                stream.consume(data.len());
                data.len() as i32
            }
        }
    }
}
