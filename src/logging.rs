//! Файловый лог рядом с DLL плюс кольцевой буфер для будущей лог-панели в оверлее.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

const RING_CAPACITY: usize = 256;

struct Sink {
    file: Option<File>,
    ring: VecDeque<String>,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

fn sink() -> &'static Mutex<Sink> {
    SINK.get_or_init(|| {
        Mutex::new(Sink {
            file: None,
            ring: VecDeque::with_capacity(RING_CAPACITY),
        })
    })
}

/// Открывает `piscatio.log` в указанной папке. Если не вышло — работаем
/// только на кольцевом буфере, молча: падать из-за лога нельзя.
pub fn init(dir: PathBuf) {
    let path = dir.join("piscatio.log");
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    if let Ok(mut s) = sink().lock() {
        s.file = file;
    }
    line("---- piscatio start ----");
}

fn stamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

pub fn line(msg: &str) {
    let text = format!("[{}] {}", stamp(), msg);
    if let Ok(mut s) = sink().lock() {
        if let Some(f) = s.file.as_mut() {
            let _ = writeln!(f, "{}", text);
            let _ = f.flush();
        }
        if s.ring.len() == RING_CAPACITY {
            s.ring.pop_front();
        }
        s.ring.push_back(text);
    }
}

/// Снимок последних строк — понадобится лог-панели оверлея.
#[allow(dead_code)]
pub fn tail(n: usize) -> Vec<String> {
    match sink().lock() {
        Ok(s) => s.ring.iter().rev().take(n).rev().cloned().collect(),
        Err(_) => Vec::new(),
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::logging::line(&format!($($arg)*)) };
}
