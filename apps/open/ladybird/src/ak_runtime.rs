//! The string runtime that Ladybird's AK crate (AK/Rust) calls into. In
//! Ladybird these functions are C++ (AK/FFIHelpers.cpp) over AK::Utf16String
//! and the fly-string table. Here they are Rust over the same memory layout:
//! a long string is a 24-byte `Utf16StringDataHeader` followed by its Latin-1
//! bytes or UTF-16 code units, and a string of fewer than one word's worth of
//! ASCII lives inline in the word itself (AK does that part on its own).
//!
//! Fly strings are interned once and never freed: their reference count starts
//! high enough that it never reaches zero. A page creates a few hundred
//! distinct names, so that costs little and keeps identity comparisons, which
//! the tree builder relies on, trivially correct.
use std::alloc::{alloc, dealloc, Layout};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const HEADER: usize = 24;
const HAS_UTF16_STORAGE: u32 = 1;
const CONTAINS_NON_ASCII: u32 = 1 << 3;
const UNKNOWN_CODE_POINT_LENGTH: u32 = u32::MAX;
const IMMORTAL: u32 = 1 << 30;

#[repr(C, align(8))]
struct Header {
    reference_count: AtomicU32,
    length_in_code_units: u32,
    length_in_code_points: AtomicU32,
    hash: AtomicU32,
    flags: AtomicU32,
}

fn layout(length: usize, latin1: bool) -> Layout {
    let units = if latin1 { length } else { length * 2 };
    Layout::from_size_align(HEADER + units.max(1), 8).expect("string layout")
}

fn header(raw: usize) -> &'static Header {
    unsafe { &*std::ptr::with_exposed_provenance::<Header>(raw) }
}

fn storage(raw: usize) -> *mut u8 {
    std::ptr::with_exposed_provenance_mut::<u8>(raw).wrapping_add(HEADER)
}

fn create(length: usize, latin1: bool, reference_count: u32) -> usize {
    let memory = unsafe { alloc(layout(length, latin1)) };
    assert!(!memory.is_null(), "out of memory for a string");
    unsafe {
        memory.cast::<Header>().write(Header {
            reference_count: AtomicU32::new(reference_count),
            length_in_code_units: length as u32,
            length_in_code_points: AtomicU32::new(UNKNOWN_CODE_POINT_LENGTH),
            hash: AtomicU32::new(0),
            flags: AtomicU32::new(if latin1 { 0 } else { HAS_UTF16_STORAGE }),
        });
    }
    memory.expose_provenance()
}

/// A long string holding these code units, one byte each when they all fit.
fn from_units(units: &[u16], reference_count: u32) -> usize {
    let latin1 = units.iter().all(|&u| u <= 0xff);
    let raw = create(units.len(), latin1, reference_count);
    if latin1 {
        for (i, &u) in units.iter().enumerate() {
            unsafe { storage(raw).add(i).write(u as u8) };
        }
        if units.iter().any(|&u| u > 0x7f) {
            header(raw).flags.fetch_or(CONTAINS_NON_ASCII, Ordering::Relaxed);
        }
    } else {
        unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), storage(raw).cast::<u16>(), units.len()) };
    }
    raw
}

/// The inline representation of a short ASCII string, as AK lays it out.
fn short(units: &[u16]) -> Option<usize> {
    if units.len() >= size_of::<usize>() || units.iter().any(|&u| u > 0x7f) {
        return None;
    }
    let mut bytes = [0u8; size_of::<usize>()];
    bytes[0] = ((units.len() as u8) << 2) | 1;
    for (i, &u) in units.iter().enumerate() {
        bytes[1 + i] = u as u8;
    }
    Some(usize::from_ne_bytes(bytes))
}

/// The code units of any raw AK string.
pub fn units_of(raw: usize) -> Vec<u16> {
    match unsafe { ak::utf16_string_units(&raw) } {
        ak::Utf16StringUnits::Ascii(b) | ak::Utf16StringUnits::Latin1(b) => b.iter().map(|&b| b as u16).collect(),
        ak::Utf16StringUnits::Utf16(u) => u.to_vec(),
    }
}

/// A raw AK string as Rust text.
pub fn string_of(raw: usize) -> String {
    String::from_utf16_lossy(&units_of(raw))
}

fn table() -> &'static Mutex<HashMap<Vec<u16>, usize>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<Vec<u16>, usize>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The fly string for these code units: the same word every time.
pub fn intern(units: &[u16]) -> usize {
    if let Some(raw) = short(units) {
        return raw;
    }
    let mut table = table().lock().unwrap();
    *table.entry(units.to_vec()).or_insert_with(|| from_units(units, IMMORTAL))
}

pub fn intern_str(s: &str) -> usize {
    intern(&s.encode_utf16().collect::<Vec<u16>>())
}

fn slice<'a, T>(data: *const T, length: usize) -> &'a [T] {
    if length == 0 { &[] } else { unsafe { std::slice::from_raw_parts(data, length) } }
}

fn well_formed(units: &[u16]) -> Vec<u16> {
    char::decode_utf16(units.iter().copied())
        .flat_map(|c| {
            let mut buf = [0u16; 2];
            c.unwrap_or('\u{fffd}').encode_utf16(&mut buf).to_vec()
        })
        .collect()
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_string_create_uninitialized(length: usize, has_latin1_storage: bool) -> usize {
    create(length, has_latin1_storage, 1)
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_string_unref(raw: usize) {
    if raw == 0 || raw & 1 != 0 {
        return;
    }
    let h = header(raw);
    if h.reference_count.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        let latin1 = h.flags.load(Ordering::Relaxed) & HAS_UTF16_STORAGE == 0;
        let length = h.length_in_code_units as usize;
        unsafe { dealloc(std::ptr::with_exposed_provenance_mut::<u8>(raw), layout(length, latin1)) };
    }
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_fly_string_from_utf8(data: *const u8, length: usize) -> usize {
    let text = String::from_utf8_lossy(slice(data, length));
    intern_str(&text)
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_fly_string_from_utf16(data: *const u16, length: usize) -> usize {
    intern(slice(data, length))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_fly_string_from_utf16_string(raw: usize) -> usize {
    intern(&units_of(raw))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_validate(data: *const u16, length: usize) -> bool {
    char::decode_utf16(slice(data, length).iter().copied()).all(|c| c.is_ok())
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_validate_prefix(data: *const u16, length: usize, valid_code_units: *mut usize) -> bool {
    let mut valid = 0;
    let mut ok = true;
    for c in char::decode_utf16(slice(data, length).iter().copied()) {
        match c {
            Ok(c) => valid += c.len_utf16(),
            Err(_) => {
                ok = false;
                break;
            }
        }
    }
    unsafe { valid_code_units.write(valid) };
    ok
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf8_length_from_valid_utf16(data: *const u16, length: usize) -> usize {
    char::decode_utf16(slice(data, length).iter().copied()).map(|c| c.map_or(3, char::len_utf8)).sum()
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_convert_valid_utf16_to_utf8(data: *const u16, length: usize, output: *mut u8) -> usize {
    let text = String::from_utf16_lossy(slice(data, length));
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), output, text.len()) };
    text.len()
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_string_from_utf8(data: *const u8, length: usize) -> usize {
    let units: Vec<u16> = String::from_utf8_lossy(slice(data, length)).encode_utf16().collect();
    short(&units).unwrap_or_else(|| from_units(&units, 1))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_utf16_string_to_well_formed(data: *const u16, length: usize) -> usize {
    let units = well_formed(slice(data, length));
    short(&units).unwrap_or_else(|| from_units(&units, 1))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_release_unused_allocator_memory() {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_set_idle_thread_memory_release(_release: extern "C" fn()) {}

// Base64 and number formatting serve LibJS, which the browser does not include.
// The HTML parser never calls them; they exist so that AK links.
#[unsafe(no_mangle)]
extern "C" fn ladybird_size_required_to_decode_base64(_data: *const u8, length: usize, _latin1: bool) -> usize {
    length
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
extern "C" fn ladybird_decode_base64_into(
    _data: *const u8,
    _length: usize,
    _latin1: bool,
    _url: bool,
    _last_chunk_handling: u8,
    _output: *mut u8,
    output_length: *mut usize,
    read: *mut usize,
) -> u8 {
    unsafe {
        output_length.write(0);
        read.write(0);
    }
    1
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_encode_base64_to_utf16(_data: *const u8, _length: usize, _url: bool, _omit_padding: bool) -> usize {
    short(&[]).unwrap()
}

fn decimal(value: f64, sign: *mut bool, fraction: *mut u64, exponent: *mut i32) {
    let text = format!("{:e}", value.abs());
    let (mantissa, exp) = text.split_once('e').unwrap_or((&text, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = mantissa.find('.').map_or(0, |p| mantissa.len() - p - 1) as i32;
    unsafe {
        sign.write(value.is_sign_negative());
        fraction.write(digits.parse().unwrap_or(0));
        exponent.write(exp.parse::<i32>().unwrap_or(0) - point);
    }
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_convert_to_decimal_exponential_form(value: f64, sign: *mut bool, fraction: *mut u64, exponent: *mut i32) {
    decimal(value, sign, fraction, exponent);
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_convert_to_decimal_exponential_form_with_trailing_zeroes(
    value: f64,
    sign: *mut bool,
    fraction: *mut u64,
    exponent: *mut i32,
) {
    decimal(value, sign, fraction, exponent);
}
