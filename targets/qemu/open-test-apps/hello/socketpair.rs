use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::thread;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

extern "C" {
    fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
}

fn ready(socket: &UnixStream, events: i16) -> i16 {
    let mut fd = PollFd { fd: socket.as_raw_fd(), events, revents: 0 };
    assert!(unsafe { poll(&mut fd, 1, 0) } >= 0);
    fd.revents
}

pub fn run() {
    pass_files();
    blocked_file_send();
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    assert_eq!(ready(&receiver, 1 | 4), 4);
    let alias = sender.try_clone().unwrap();
    drop(sender);
    sender = alias;
    sender.set_nonblocking(true).unwrap();
    let mut byte = [0; 1];
    assert_eq!(sender.read(&mut byte).unwrap_err().kind(), ErrorKind::WouldBlock);
    let chunk = [0x53; 4096];
    let mut queued = 0;
    loop {
        match sender.write(&chunk) {
            Ok(n) => { assert!(n > 0); queued += n; }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("send: {e}"),
        }
        assert!(queued < 1 << 20);
    }
    assert_eq!(ready(&sender, 4), 0);
    assert_ne!(ready(&receiver, 1) & 1, 0);
    sender.set_nonblocking(false).unwrap();
    let writer = thread::spawn(move || {
        sender.write_all(b"end").unwrap();
        sender.shutdown(Shutdown::Write).unwrap();
        sender.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [42]);
        drop(sender);
    });
    let mut data = Vec::new();
    receiver.read_to_end(&mut data).unwrap();
    assert_eq!(data.len(), queued + 3);
    assert!(data[..queued].iter().all(|&b| b == 0x53));
    assert_eq!(&data[queued..], b"end");
    receiver.write_all(&[42]).unwrap();
    writer.join().unwrap();
    assert_ne!(ready(&receiver, 1) & 0x10, 0);
    assert_eq!(receiver.write(&[42]).unwrap_err().kind(), ErrorKind::BrokenPipe);
    println!("socketpair duplex, backpressure, EOF and hangup passed");
}

#[repr(C)]
struct IoVec {
    base: *mut u8,
    len: usize,
}

#[repr(C)]
struct Message {
    name: *mut u8,
    name_size: u32,
    vecs: *mut IoVec,
    num_vecs: usize,
    control: *mut u32,
    control_len: usize,
    flags: i32,
}

impl Message {
    fn new(vecs: &mut [IoVec], control: &mut [u32]) -> Self {
        Self { name: std::ptr::null_mut(), name_size: 0, vecs: vecs.as_mut_ptr(), num_vecs: vecs.len(),
               control: control.as_mut_ptr(), control_len: std::mem::size_of_val(control), flags: 0 }
    }
}

extern "C" {
    fn sendmsg(fd: i32, msg: *const Message, flags: i32) -> isize;
    fn recvmsg(fd: i32, msg: *mut Message, flags: i32) -> isize;
    fn fcntl(fd: i32, cmd: i32, arg: i32) -> i32;
}

fn pass_files() {
    use std::fs::{File, OpenOptions};
    use std::io::{Seek, SeekFrom};
    use std::os::fd::FromRawFd;

    let (mut tx, mut rx) = UnixStream::pair().unwrap();
    let mut file = OpenOptions::new().create(true).truncate(true).read(true).write(true)
        .open("/tmp/rights").unwrap();
    file.write_all(b"violet").unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let fd = file.as_raw_fd() as u32;
    let mut control = [20, 1, 1, fd, 0x7fff_ffff];
    let mut pairs = [*b"ab", *b"cd"];
    let mut vecs = pairs.each_mut().map(|p| IoVec { base: p.as_mut_ptr(), len: p.len() });
    let msg = Message::new(&mut vecs, &mut control);
    let send = || unsafe { sendmsg(tx.as_raw_fd(), &msg, 0x4000) };
    assert_eq!(send(), -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(9));
    assert_eq!(ready(&rx, 1), 0);
    control[4] = tx.as_raw_fd() as u32;
    assert_eq!(send(), -1); // Cyclic local socket rights are unsupported.
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(95));
    control[4] = fd;
    assert_eq!(send(), 4);
    let original_alias = file.try_clone().unwrap();
    drop(file);
    let mut received = [0; 5];
    let mut payload = [0u8; 2];
    let mut vecs = payload.each_mut().map(|b| IoVec { base: b as *mut u8, len: 1 });
    let mut get = Message::new(&mut vecs, &mut received);
    get.control = 1usize as *mut u32;
    assert_eq!(unsafe { recvmsg(rx.as_raw_fd(), &mut get, 0) }, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(14));
    let mut get = Message::new(&mut vecs, &mut received);
    assert_eq!(unsafe { recvmsg(rx.as_raw_fd(), &mut get, 0x40000000) }, 2);
    assert_eq!((payload, received[..3].to_vec(), get.control_len, get.flags), (*b"ab", vec![20, 1, 1], 20, 0));
    let mut first = unsafe { File::from_raw_fd(received[3] as i32) };
    let mut second = unsafe { File::from_raw_fd(received[4] as i32) };
    assert_eq!(unsafe { fcntl(first.as_raw_fd(), 1, 0) }, 1);
    let mut word = [0; 3];
    first.read_exact(&mut word).unwrap();
    assert_eq!(word, *b"vio");
    second.read_exact(&mut word).unwrap();
    assert_eq!(word, *b"let");
    // The remaining payload carries no more descriptors.
    let mut get = Message::new(&mut vecs, &mut received);
    assert_eq!(unsafe { recvmsg(rx.as_raw_fd(), &mut get, 0) }, 2);
    assert_eq!((payload, get.control_len, get.flags), (*b"cd", 0, 0));
    drop((first, second));

    let mut byte = [0; 1];
    let mut vecs = [IoVec { base: byte.as_mut_ptr(), len: 1 }];
    let fd = original_alias.as_raw_fd() as u32;
    let mut control = [20, 1, 1, fd, fd];
    let msg = Message::new(&mut vecs, &mut control);
    assert_eq!(unsafe { sendmsg(tx.as_raw_fd(), &msg, 0) }, 1);
    drop(original_alias); // The queue keeps the open description alive.
    let mut short = [0; 4];
    let mut get = Message::new(&mut vecs, &mut short);
    assert_eq!(unsafe { recvmsg(rx.as_raw_fd(), &mut get, 0) }, 1);
    assert_eq!((get.control_len, get.flags), (16, 8)); // one fd delivered, MSG_CTRUNC
    let mut received = unsafe { File::from_raw_fd(short[3] as i32) };
    assert_eq!(unsafe { fcntl(received.as_raw_fd(), 1, 0) }, 0);
    received.rewind().unwrap();
    received.read_exact(&mut word).unwrap();
    assert_eq!(word, *b"vio");
    // Blocking receive is resumed as recvmsg, preserving its control buffer.
    let peer = thread::spawn(move || {
        let mut buf = [0];
        let mut v = [IoVec { base: buf.as_mut_ptr(), len: 1 }];
        let mut ctrl = [0; 4];
        let mut msg = Message::new(&mut v, &mut ctrl);
        assert_eq!(unsafe { recvmsg(rx.as_raw_fd(), &mut msg, 0) }, 1);
        let file = unsafe { File::from_raw_fd(ctrl[3] as i32) };
        assert_eq!(file.metadata().unwrap().len(), 6);
        rx.write_all(b"y").unwrap();
    });
    thread::sleep(std::time::Duration::from_millis(20));
    let mut ctrl = [16, 1, 1, received.as_raw_fd() as u32];
    let msg = Message::new(&mut vecs, &mut ctrl);
    assert_eq!(unsafe { sendmsg(tx.as_raw_fd(), &msg, 0) }, 1);
    tx.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"y");
    peer.join().unwrap();
    println!("SCM_RIGHTS file ownership, truncation, copyout rollback and blocking receive passed");
}

fn blocked_file_send() {
    use std::fs::File;
    use std::os::fd::FromRawFd;
    use std::sync::mpsc;

    let (mut tx, mut rx) = UnixStream::pair().unwrap();
    tx.set_nonblocking(true).unwrap();
    let mut queued = 0;
    let padding = [0xa5u8; 4096];
    loop {
        match tx.write(&padding) {
            Ok(n) => { assert!(n > 0); queued += n; }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("fill stream: {e}"),
        }
    }
    tx.set_nonblocking(false).unwrap();
    let (ready, start) = mpsc::channel();
    let writer = thread::spawn(move || {
        let passed = File::open("/tmp/rights").unwrap();
        let mut payload = [42u8];
        let mut io = [IoVec { base: payload.as_mut_ptr(), len: 1 }];
        let mut control = [16, 1, 1, passed.as_raw_fd() as u32];
        let msg = Message::new(&mut io, &mut control);
        ready.send(()).unwrap();
        assert_eq!(unsafe { sendmsg(tx.as_raw_fd(), &msg, 0) }, 1);
    });
    start.recv().unwrap();
    thread::sleep(std::time::Duration::from_millis(20));
    let mut prefix = vec![0; queued];
    rx.read_exact(&mut prefix).unwrap();
    assert!(prefix.iter().all(|&b| b == 0xa5));
    writer.join().unwrap();
    let mut payload = [0u8];
    let mut io = [IoVec { base: payload.as_mut_ptr(), len: 1 }];
    let mut control = [0; 4];
    let mut msg = Message::new(&mut io, &mut control);
    assert_eq!(unsafe { recvmsg(rx.as_raw_fd(), &mut msg, 0) }, 1);
    assert_eq!((payload, msg.control_len, msg.flags), ([42], 16, 0));
    let mut file = unsafe { File::from_raw_fd(control[3] as i32) };
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "violet");
}
