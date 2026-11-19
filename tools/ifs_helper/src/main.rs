//! Reads Call of Duty Online `.ifs` archives through the game's own `IFS2.dll`.
//!
//! The DLL is 32-bit and belongs to the game; this program ships without it and loads the copy from the user's
//! installation (`<game>/IIPS/IFS2.dll`). It only runs against a DLL whose SHA-256 it knows, because the functions are
//! called by their position inside that build (the same calls Maurice Heumann's MIT-licensed `ifs-tool` makes,
//! https://github.com/momo5502/ifs-tool, from which the function positions and calling conventions come). Nothing about
//! the archive format or its protection is reimplemented here: the DLL does all the reading.
//!
//! Protocol on stdin / stdout, one command per line:
//!   OPEN <path>   -> "OK\n" or "ERR <why>\n"        (opens an archive; the previous one is closed)
//!   READ <name>   -> "OK <length>\n" + bytes, or "ERR <why>\n"
//!   QUIT
//! The archive's file list is the file `(listfile)`.

#![cfg(all(windows, target_arch = "x86"))]

use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::io::{BufRead, Write};

/// IFS2.dll of build 1.2.1.12 and of the 3.19 client (identical files).
const KNOWN_DLLS: [&str; 1] = ["5b0324e0b2ba3545a6c02626287d4fbc6a3116594744f4647333982d85508421"];

const OPEN_ARCHIVE: usize = 0x163E0;
const CLOSE_FILE: usize = 0x210A0;
const READ_FILE: usize = 0x22570;
const OPEN_FILE: usize = 0x1FED0;
/// Offset of the archive's buffer size field.
const ALLOCATION_SIZE: usize = 0x138;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const u8) -> usize;
    fn SetDllDirectoryA(path: *const u8) -> i32;
}

struct Dll {
    base: usize,
}

impl Dll {
    unsafe fn open_archive(&self, path: &CString, flag: i32) -> usize {
        let f: extern "system" fn(*const u8, i32) -> usize = std::mem::transmute(self.base + OPEN_ARCHIVE);
        f(path.as_ptr() as *const u8, flag)
    }

    unsafe fn open_file(&self, archive: usize, name: &CString) -> usize {
        let f: extern "system" fn(usize, *const u8, u32, *mut usize, u32) -> u8 = std::mem::transmute(self.base + OPEN_FILE);
        let mut handle = 0usize;
        f(archive, name.as_ptr() as *const u8, 1, &mut handle, 0);
        handle
    }

    /// `ecx` = handle, `edx` = buffer, then size, length pointer and a flag on the stack (callee cleans up).
    unsafe fn read_file(&self, handle: usize, buffer: *mut u8, size: usize, length: *mut usize) {
        let f: extern "fastcall" fn(usize, *mut u8, usize, *mut usize, i32) = std::mem::transmute(self.base + READ_FILE);
        f(handle, buffer, size, length, 1);
    }

    /// The close routine takes its handle in `esi`.
    unsafe fn close(&self, handle: usize) {
        let target = self.base + CLOSE_FILE;
        std::arch::asm!("push esi", "mov esi, {h}", "call {f}", "pop esi", h = in(reg) handle, f = in(reg) target, clobber_abi("C"));
    }
}

fn main() {
    let dll_path = std::env::args().nth(1).unwrap_or_default();
    let out = std::io::stdout();
    let mut out = out.lock();
    let bytes = match std::fs::read(&dll_path) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(out, "ERR cannot read {dll_path}: {e}");
            return;
        }
    };
    let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if !KNOWN_DLLS.contains(&hash.as_str()) {
        let _ = writeln!(out, "ERR unsupported IFS2.dll (sha256 {hash})");
        return;
    }
    drop(bytes);
    if let Some(dir) = std::path::Path::new(&dll_path).parent() {
        let _ = std::env::set_current_dir(dir);
    }
    let c_path = CString::new(dll_path.clone()).unwrap();
    if let Some(dir) = std::path::Path::new(&dll_path).parent() {
        if let Ok(d) = CString::new(dir.to_string_lossy().as_bytes()) {
            unsafe { SetDllDirectoryA(d.as_ptr() as *const u8) };
        }
    }
    let base = unsafe { LoadLibraryA(c_path.as_ptr() as *const u8) };
    if base == 0 {
        let _ = writeln!(out, "ERR cannot load the DLL");
        return;
    }
    let _ = writeln!(out, "OK ready");
    let _ = out.flush();
    let dll = Dll { base };
    let mut archive = 0usize;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
        if std::env::var_os("IFS_HELPER_TRACE").is_some() {
            eprintln!("> {line}");
        }
        let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match cmd {
            "OPEN" => {
                let (flag, arg) = match arg.split_once(' ') {
                    Some((f, rest)) if f.len() <= 2 && f.parse::<i32>().is_ok() => (f.parse().unwrap(), rest),
                    _ => (0, arg),
                };
                let Ok(p) = CString::new(arg) else {
                    let _ = writeln!(out, "ERR bad path");
                    continue;
                };
                if archive != 0 {
                    // release the previous archive, or handles pile up when many are opened in turn
                    unsafe { dll.close(archive) };
                }
                archive = unsafe { dll.open_archive(&p, flag) };
                let _ = writeln!(out, "{}", if archive != 0 { "OK" } else { "ERR cannot open the archive" });
            }
            "READ" => {
                if archive == 0 {
                    let _ = writeln!(out, "ERR no archive is open");
                    continue;
                }
                let Ok(name) = CString::new(arg) else {
                    let _ = writeln!(out, "ERR bad name");
                    continue;
                };
                unsafe {
                    let file = dll.open_file(archive, &name);
                    if file == 0 {
                        let _ = writeln!(out, "ERR no such file");
                        continue;
                    }
                    // the archive's buffer size caps one read; a larger file takes several, each continuing where
                    // the last stopped, until one returns less than a full buffer
                    let size = std::ptr::read((archive + ALLOCATION_SIZE) as *const usize).clamp(1, 1 << 26);
                    let mut buf = vec![0u8; size];
                    let mut all: Vec<u8> = Vec::new();
                    loop {
                        let mut length = 0usize;
                        dll.read_file(file, buf.as_mut_ptr(), buf.len(), &mut length);
                        if length > buf.len() {
                            all.clear();
                            break;
                        }
                        all.extend_from_slice(&buf[..length]);
                        if length < buf.len() || all.len() > (1 << 30) {
                            break;
                        }
                    }
                    dll.close(file);
                    let length = all.len();
                    if length == 0 {
                        let _ = writeln!(out, "ERR empty or oversized read");
                        continue;
                    }
                    let buf = all;
                    let _ = writeln!(out, "OK {length}");
                    let _ = out.write_all(&buf[..length]);
                }
            }
            "QUIT" => break,
            _ => {
                let _ = writeln!(out, "ERR unknown command");
            }
        }
        let _ = out.flush();
    }
    // the archive handle is closed by process exit
    let _ = archive;
}
