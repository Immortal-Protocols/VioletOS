// Guest side of the DNS, TCP stream, half-close and EOF test.
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
fn main() {
    let mut socket = TcpStream::connect(("network-test.violet", 80)).expect("DNS and TCP connect");
    socket.write_all(b"Violet socket probe\n").unwrap();
    socket.shutdown(Shutdown::Write).unwrap();
    let mut reply = String::new();
    socket.read_to_string(&mut reply).unwrap();
    assert_eq!(reply, "Violet socket reply\n");
    println!("AArch64 VioletOS network test passed");
}
