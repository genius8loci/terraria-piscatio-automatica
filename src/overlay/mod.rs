//! Оверлей поверх игры: перехват `IDirect3DDevice9::EndScene` и свой мини-рендер.
//!
//! Terraria — XNA 4.0, то есть Direct3D9. Адрес девайса берём не из игры,
//! а из временного «пустого» девайса: у всех девайсов одного d3d9.dll общая
//! vtable, поэтому подмена слота действует и на девайс игры. На Vista+
//! в ходу два варианта (обычный и Ex), поэтому цепляем оба, если их таблицы
//! различаются, а нужный оригинал выбираем по vtable вызвавшего девайса.

mod font;

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D9::{
    D3DBLEND_INVSRCALPHA, D3DBLEND_SRCALPHA, D3DCREATE_SOFTWARE_VERTEXPROCESSING, D3DCULL_NONE,
    D3DDEVTYPE_HAL, D3DFMT_A8R8G8B8, D3DFMT_UNKNOWN, D3DFVF_DIFFUSE, D3DFVF_TEX1, D3DFVF_XYZRHW,
    D3DLOCKED_RECT, D3DPOOL_DEFAULT, D3DPRESENT_PARAMETERS, D3DPT_TRIANGLELIST,
    D3DRS_ALPHABLENDENABLE, D3DRS_CULLMODE, D3DRS_DESTBLEND, D3DRS_FOGENABLE, D3DRS_LIGHTING,
    D3DRS_SCISSORTESTENABLE, D3DRS_SRCBLEND, D3DRS_STENCILENABLE, D3DRS_ZENABLE, D3DSBT_ALL,
    D3DSWAPEFFECT_DISCARD, D3DTA_DIFFUSE, D3DTA_TEXTURE, D3DTOP_MODULATE, D3DTOP_SELECTARG1,
    D3DTSS_ALPHAARG1, D3DTSS_ALPHAARG2, D3DTSS_ALPHAOP, D3DTSS_COLORARG1, D3DTSS_COLORARG2,
    D3DTSS_COLOROP, D3DUSAGE_DYNAMIC, Direct3DCreate9, Direct3DCreate9Ex, IDirect3D9, IDirect3D9Ex,
    IDirect3DDevice9, IDirect3DStateBlock9, IDirect3DTexture9,
};
use windows::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
use windows::core::{HRESULT, Interface};

use crate::SHOW_UI;
use font::FontAtlas;

const D3D_SDK_VERSION: u32 = 32;
const SLOT_RESET: usize = 16;
const SLOT_END_SCENE: usize = 42;

const FVF: u32 = D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1;

// Палитра под инвентарные панели Terraria.
const COLOR_BORDER: u32 = 0xFF_1B1B38;
const COLOR_FRAME: u32 = 0xFF_5A5CB8;
const COLOR_BACK: u32 = 0xE6_2E3070;
const COLOR_TITLE: u32 = 0xFF_FFD75E;
const COLOR_TEXT: u32 = 0xFF_E4E4F2;

const PANEL_X: f32 = 24.0;
const PANEL_Y: f32 = 24.0;
const PANEL_W: f32 = 430.0;
const PADDING: f32 = 10.0;

type FnEndScene = unsafe extern "system" fn(*mut c_void) -> HRESULT;
type FnReset = unsafe extern "system" fn(*mut c_void, *mut D3DPRESENT_PARAMETERS) -> HRESULT;

/// До двух перехваченных таблиц: обычная и Ex.
const MAX_TABLES: usize = 2;
static TABLES: [AtomicUsize; MAX_TABLES] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static ORIGINAL_END_SCENE: [AtomicUsize; MAX_TABLES] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static ORIGINAL_RESET: [AtomicUsize; MAX_TABLES] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static INSTALLED: AtomicBool = AtomicBool::new(false);
static FIRST_FRAME_LOGGED: AtomicBool = AtomicBool::new(false);

static SNAPSHOT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static FONT: OnceLock<Option<FontAtlas>> = OnceLock::new();

struct Resources {
    texture: IDirect3DTexture9,
    block: IDirect3DStateBlock9,
}

thread_local! {
    /// Ресурсы живут на потоке рендера — там же, где вызывается EndScene.
    static RESOURCES: RefCell<Option<Resources>> = const { RefCell::new(None) };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    z: f32,
    rhw: f32,
    color: u32,
    u: f32,
    v: f32,
}

/// Обновить строки, которые показывает панель.
pub fn set_lines(lines: Vec<String>) {
    if let Ok(mut slot) = SNAPSHOT.lock() {
        *slot = lines;
    }
}

// ---------------------------------------------------------------------------
// Установка хука
// ---------------------------------------------------------------------------

pub fn install() -> bool {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }

    let mut hooked = 0usize;
    for vtable in [dummy_vtable_ex(), dummy_vtable()].into_iter().flatten() {
        if hooked >= MAX_TABLES {
            break;
        }
        let address = vtable as usize;
        if TABLES.iter().any(|t| t.load(Ordering::Relaxed) == address) {
            continue; // обычный и Ex дали одну и ту же таблицу
        }

        let end_scene = unsafe {
            patch_slot(
                vtable,
                SLOT_END_SCENE,
                hook_end_scene as FnEndScene as usize,
            )
        };
        let reset = unsafe { patch_slot(vtable, SLOT_RESET, hook_reset as FnReset as usize) };
        match (end_scene, reset) {
            (Some(original_end), Some(original_reset)) => {
                TABLES[hooked].store(address, Ordering::SeqCst);
                ORIGINAL_END_SCENE[hooked].store(original_end, Ordering::SeqCst);
                ORIGINAL_RESET[hooked].store(original_reset, Ordering::SeqCst);
                crate::log!("оверлей: перехвачена vtable 0x{address:08X}");
                hooked += 1;
            }
            _ => crate::log!("оверлей: не удалось пропатчить vtable 0x{address:08X}"),
        }
    }

    if hooked == 0 {
        crate::log!("оверлей: ни одной vtable перехватить не удалось");
        INSTALLED.store(false, Ordering::SeqCst);
        return false;
    }

    if FONT.get_or_init(font::build).is_none() {
        crate::log!("оверлей: атлас шрифта построить не вышло, текста не будет");
    }
    true
}

unsafe fn patch_slot(vtable: *mut usize, index: usize, replacement: usize) -> Option<usize> {
    unsafe {
        let slot = vtable.add(index);
        let size = size_of::<usize>();
        let mut previous = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(slot as *const c_void, size, PAGE_READWRITE, &mut previous).ok()?;
        let original = *slot;
        *slot = replacement;
        let mut restored = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(slot as *const c_void, size, previous, &mut restored);
        Some(original)
    }
}

fn present_parameters() -> D3DPRESENT_PARAMETERS {
    D3DPRESENT_PARAMETERS {
        Windowed: true.into(),
        SwapEffect: D3DSWAPEFFECT_DISCARD,
        BackBufferFormat: D3DFMT_UNKNOWN,
        hDeviceWindow: unsafe { GetDesktopWindow() },
        ..Default::default()
    }
}

fn dummy_vtable() -> Option<*mut usize> {
    unsafe {
        let d3d: IDirect3D9 = Direct3DCreate9(D3D_SDK_VERSION)?;
        let mut parameters = present_parameters();
        let mut device: Option<IDirect3DDevice9> = None;
        d3d.CreateDevice(
            0,
            D3DDEVTYPE_HAL,
            GetDesktopWindow(),
            D3DCREATE_SOFTWARE_VERTEXPROCESSING as u32,
            &mut parameters,
            &mut device,
        )
        .ok()?;
        let device = device?;
        Some(*(device.as_raw() as *mut *mut usize))
    }
}

fn dummy_vtable_ex() -> Option<*mut usize> {
    unsafe {
        let d3d: IDirect3D9Ex = Direct3DCreate9Ex(D3D_SDK_VERSION).ok()?;
        let mut parameters = present_parameters();
        let mut device = None;
        d3d.CreateDeviceEx(
            0,
            D3DDEVTYPE_HAL,
            GetDesktopWindow(),
            D3DCREATE_SOFTWARE_VERTEXPROCESSING as u32,
            &mut parameters,
            std::ptr::null_mut(),
            &mut device,
        )
        .ok()?;
        let device = device?;
        Some(*(device.as_raw() as *mut *mut usize))
    }
}

fn table_index(device: *mut c_void) -> Option<usize> {
    let vtable = unsafe { *(device as *const usize) };
    (0..MAX_TABLES).find(|&i| TABLES[i].load(Ordering::Relaxed) == vtable)
}

// ---------------------------------------------------------------------------
// Перехватчики
// ---------------------------------------------------------------------------

unsafe extern "system" fn hook_end_scene(device: *mut c_void) -> HRESULT {
    let index = table_index(device);

    if !FIRST_FRAME_LOGGED.swap(true, Ordering::SeqCst) {
        crate::log!("оверлей: первый кадр перехвачен, рендер работает");
    }

    if SHOW_UI.load(Ordering::Relaxed) {
        // Паника внутри чужого кадра убьёт игру — гасим на месте.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe { draw(device) }));
    }

    match index.map(|i| ORIGINAL_END_SCENE[i].load(Ordering::Relaxed)) {
        Some(original) if original != 0 => unsafe {
            let call: FnEndScene = std::mem::transmute(original);
            call(device)
        },
        _ => HRESULT(0),
    }
}

unsafe extern "system" fn hook_reset(
    device: *mut c_void,
    parameters: *mut D3DPRESENT_PARAMETERS,
) -> HRESULT {
    // Ресурсы в D3DPOOL_DEFAULT обязаны быть освобождены до Reset.
    RESOURCES.with(|slot| slot.borrow_mut().take());

    match table_index(device).map(|i| ORIGINAL_RESET[i].load(Ordering::Relaxed)) {
        Some(original) if original != 0 => unsafe {
            let call: FnReset = std::mem::transmute(original);
            call(device, parameters)
        },
        _ => HRESULT(0),
    }
}

// ---------------------------------------------------------------------------
// Рендер
// ---------------------------------------------------------------------------

unsafe fn draw(raw: *mut c_void) {
    let Some(device) = (unsafe { IDirect3DDevice9::from_raw_borrowed(&raw) }) else {
        return;
    };

    ensure_resources(device);

    let lines = SNAPSHOT.lock().map(|l| l.clone()).unwrap_or_default();
    let font = FONT.get().and_then(|f| f.as_ref());

    let line_height = font.map(|f| f.cell_h as f32).unwrap_or(18.0) + 2.0;
    let body = lines.len().max(1) as f32 * line_height;
    let panel_h = PADDING * 2.0 + line_height + 6.0 + body;

    let mut quads: Vec<Vertex> = Vec::with_capacity(64);
    // Рамка и фон.
    push_rect(
        &mut quads,
        PANEL_X - 2.0,
        PANEL_Y - 2.0,
        PANEL_W + 4.0,
        panel_h + 4.0,
        COLOR_BORDER,
    );
    push_rect(
        &mut quads,
        PANEL_X - 1.0,
        PANEL_Y - 1.0,
        PANEL_W + 2.0,
        panel_h + 2.0,
        COLOR_FRAME,
    );
    push_rect(&mut quads, PANEL_X, PANEL_Y, PANEL_W, panel_h, COLOR_BACK);

    let mut glyphs: Vec<Vertex> = Vec::with_capacity(1024);
    if let Some(atlas) = font {
        let mut y = PANEL_Y + PADDING;
        push_text(
            &mut glyphs,
            atlas,
            PANEL_X + PADDING,
            y,
            "piscatio",
            COLOR_TITLE,
        );
        y += line_height + 6.0;
        for line in &lines {
            push_text(&mut glyphs, atlas, PANEL_X + PADDING, y, line, COLOR_TEXT);
            y += line_height;
        }
    }

    RESOURCES.with(|slot| {
        let borrowed = slot.borrow();
        let Some(resources) = borrowed.as_ref() else {
            return;
        };
        unsafe {
            let _ = resources.block.Capture();
            apply_states(device);

            if !quads.is_empty() {
                let _ = device.SetTexture(0, None);
                let _ = device.SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1.0 as u32);
                let _ = device.SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
                let _ = device.SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1.0 as u32);
                let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
                let _ = device.DrawPrimitiveUP(
                    D3DPT_TRIANGLELIST,
                    (quads.len() / 3) as u32,
                    quads.as_ptr() as *const c_void,
                    size_of::<Vertex>() as u32,
                );
            }

            if !glyphs.is_empty() {
                let _ = device.SetTexture(0, &resources.texture);
                let _ = device.SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE.0 as u32);
                let _ = device.SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
                let _ = device.SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
                let _ = device.SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_MODULATE.0 as u32);
                let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
                let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE);
                let _ = device.DrawPrimitiveUP(
                    D3DPT_TRIANGLELIST,
                    (glyphs.len() / 3) as u32,
                    glyphs.as_ptr() as *const c_void,
                    size_of::<Vertex>() as u32,
                );
            }

            let _ = resources.block.Apply();
        }
    });
}

unsafe fn apply_states(device: &IDirect3DDevice9) {
    unsafe {
        let _ = device.SetVertexShader(None);
        let _ = device.SetPixelShader(None);
        let _ = device.SetFVF(FVF);
        let _ = device.SetRenderState(D3DRS_ZENABLE, 0);
        let _ = device.SetRenderState(D3DRS_LIGHTING, 0);
        let _ = device.SetRenderState(D3DRS_FOGENABLE, 0);
        let _ = device.SetRenderState(D3DRS_STENCILENABLE, 0);
        let _ = device.SetRenderState(D3DRS_SCISSORTESTENABLE, 0);
        let _ = device.SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE.0 as u32);
        let _ = device.SetRenderState(D3DRS_ALPHABLENDENABLE, 1);
        let _ = device.SetRenderState(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA.0 as u32);
        let _ = device.SetRenderState(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA.0 as u32);
    }
}

fn ensure_resources(device: &IDirect3DDevice9) {
    RESOURCES.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let Some(Some(atlas)) = FONT.get() else {
            return;
        };
        let Some(texture) = create_font_texture(device, atlas) else {
            crate::log!("оверлей: не удалось создать текстуру шрифта");
            return;
        };
        let block = match unsafe { device.CreateStateBlock(D3DSBT_ALL) } {
            Ok(b) => b,
            Err(e) => {
                crate::log!("оверлей: CreateStateBlock не удался: {e}");
                return;
            }
        };
        *slot.borrow_mut() = Some(Resources { texture, block });
        crate::log!("оверлей: ресурсы созданы");
    });
}

fn create_font_texture(device: &IDirect3DDevice9, atlas: &FontAtlas) -> Option<IDirect3DTexture9> {
    unsafe {
        let mut texture: Option<IDirect3DTexture9> = None;
        device
            .CreateTexture(
                atlas.width,
                atlas.height,
                1,
                D3DUSAGE_DYNAMIC as u32,
                D3DFMT_A8R8G8B8,
                D3DPOOL_DEFAULT,
                &mut texture,
                std::ptr::null_mut(),
            )
            .ok()?;
        let texture = texture?;

        let mut locked = D3DLOCKED_RECT::default();
        texture
            .LockRect(0, &mut locked, std::ptr::null::<RECT>(), 0)
            .ok()?;
        for row in 0..atlas.height as usize {
            let destination =
                (locked.pBits as *mut u8).add(row * locked.Pitch as usize) as *mut u32;
            let source = atlas.pixels.as_ptr().add(row * atlas.width as usize);
            std::ptr::copy_nonoverlapping(source, destination, atlas.width as usize);
        }
        texture.UnlockRect(0).ok()?;
        Some(texture)
    }
}

fn push_rect(out: &mut Vec<Vertex>, x: f32, y: f32, w: f32, h: f32, color: u32) {
    push_quad(out, x, y, w, h, color, 0.0, 0.0, 0.0, 0.0);
}

#[allow(clippy::too_many_arguments)]
fn push_quad(
    out: &mut Vec<Vertex>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
) {
    let make = |x: f32, y: f32, u: f32, v: f32| Vertex {
        x: x - 0.5,
        y: y - 0.5,
        z: 0.0,
        rhw: 1.0,
        color,
        u,
        v,
    };
    let top_left = make(x, y, u0, v0);
    let top_right = make(x + w, y, u1, v0);
    let bottom_right = make(x + w, y + h, u1, v1);
    let bottom_left = make(x, y + h, u0, v1);

    out.extend_from_slice(&[top_left, top_right, bottom_right]);
    out.extend_from_slice(&[top_left, bottom_right, bottom_left]);
}

fn push_text(out: &mut Vec<Vertex>, atlas: &FontAtlas, x: f32, y: f32, text: &str, color: u32) {
    let mut pen = x;
    let tw = atlas.width as f32;
    let th = atlas.height as f32;
    for ch in text.chars() {
        if let Some((cx, cy)) = atlas.cell(ch) {
            push_quad(
                out,
                pen,
                y,
                atlas.cell_w as f32,
                atlas.cell_h as f32,
                color,
                cx as f32 / tw,
                cy as f32 / th,
                (cx + atlas.cell_w) as f32 / tw,
                (cy + atlas.cell_h) as f32 / th,
            );
        }
        pen += atlas.advance as f32;
    }
}
