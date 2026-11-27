//! The AArch64 image can install the upstream LibWeb document process.
//! Input and serialized output stay inside the VioletOS guest.
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

const HELPER: &str = "/usr/libexec/ladybird-dom";

pub fn available() -> bool {
    cfg!(all(target_os = "linux", target_arch = "aarch64")) && std::path::Path::new(HELPER).is_file()
}

pub fn execute(url: &str, source: &str) -> io::Result<String> {
    let mut child = Command::new(HELPER)
        .args(["--html", "--url", url, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    // LibWeb consumes the complete input document before serializing its DOM.
    let input = child.stdin.take().unwrap().write_all(source.as_bytes());
    let mut document = String::new();
    let output = child.stdout.take().unwrap().take(violet::http::MAX_BODY as u64 + 1).read_to_string(&mut document);
    let status = child.wait()?;
    input?;
    let size = output?;
    if !status.success() { return Err(io::Error::other(format!("LibWeb exited with {status}"))); }
    if size > violet::http::MAX_BODY { return Err(io::Error::other("document exceeds the size limit")); }
    Ok(document)
}
