// The AArch64 bring-up test compiles its sample with the upstream LibJS frontend.
use libjs_rust::{ast::ProgramType, compile};

fn main() {
    let source = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "const answer = 6 * 7;".into());
    let parsed = compile::parse(&ak::Utf16String::from_utf8(&source), ProgramType::Script, 1);
    if parsed.has_errors() {
        eprintln!("LibJS rejected the script");
        std::process::exit(1);
    }
    let compiled = compile::compile_script(parsed);
    println!("LibJS compiled {} bytes of bytecode", compiled.executable.bytecode.len());
}
