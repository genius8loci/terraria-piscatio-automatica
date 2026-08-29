//! Файловый лог рядом с DLL плюс кольцевой буфер для будущей лог-панели в оверлее.
//!
//! Лог пишется в том числе из детура на игровом потоке, поэтому здесь нет
//! ничего, что могло бы застрять: замок берётся коротко, отравленный мьютекс
//! разотравляется на месте (данные — строки, после паники они не становятся
//! опасными, только устаревшими), а `flush` не зовётся вовсе: `std::fs::File`
//! не буферизует, запись и так уходит в систему сразу.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

const RING_CAPACITY: usize = 256;
/// Предел размера файла. Сессия рыбалки на несколько часов пишет много,
/// и без предела лог рос бы бесконечно; при переполнении текущий файл
/// уезжает в `piscatio.log.old`, а новый начинается с чистого листа.
const SIZE_LIMIT: u64 = 4 * 1024 * 1024;

struct Sink {
    path: Option<PathBuf>,
    file: Option<File>,
    written: u64,
    ring: VecDeque<String>,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

fn sink() -> MutexGuard<'static, Sink> {
    let mutex = SINK.get_or_init(|| {
        Mutex::new(Sink {
            path: None,
            file: None,
            written: 0,
            ring: VecDeque::with_capacity(RING_CAPACITY),
        })
    });
    // Паника под этим замком оставляет за собой лишь недописанную строку,
    // а отказ логировать после неё стоил бы нам диагностики ровно тогда,
    // когда она нужнее всего.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Открывает `piscatio.log` в указанной папке. Если не вышло — работаем
/// только на кольцевом буфере, молча: падать из-за лога нельзя.
pub fn init(dir: PathBuf) {
    let path = dir.join("piscatio.log");
    {
        let mut s = sink();
        s.path = Some(path.clone());
        s.file = None;
        s.written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if s.written >= SIZE_LIMIT {
            rotate(&mut s);
        } else {
            s.file = open(&path);
        }
    }
    line("---- piscatio start ----");
}

fn open(path: &PathBuf) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Уводит переполненный лог в `.old` и начинает новый.
fn rotate(s: &mut Sink) {
    let Some(path) = s.path.clone() else {
        return;
    };
    // Файл должен быть закрыт до переименования: иначе Windows его не отдаст.
    s.file = None;
    let _ = std::fs::rename(&path, path.with_extension("log.old"));
    s.written = 0;
    s.file = open(&path);
}

fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

pub fn line(msg: &str) {
    let text = format!("[{}] {}", stamp(), msg);
    let mut s = sink();
    if let Some(f) = s.file.as_mut()
        && writeln!(f, "{text}").is_ok()
    {
        s.written += text.len() as u64 + 2;
        if s.written >= SIZE_LIMIT {
            rotate(&mut s);
        }
    }
    if s.ring.len() == RING_CAPACITY {
        s.ring.pop_front();
    }
    s.ring.push_back(text);
}

/// Снимок последних строк — понадобится лог-панели оверлея.
#[allow(dead_code)]
pub fn tail(n: usize) -> Vec<String> {
    let s = sink();
    s.ring.iter().rev().take(n).rev().cloned().collect()
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::logging::line(&format!($($arg)*)) };
}
