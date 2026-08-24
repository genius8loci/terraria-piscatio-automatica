//! Доступ к состоянию игры через рефлексию .NET прямо из натива.
//!
//! Terraria — managed-процесс, поэтому CLR уже поднята, а mscorlib COM-visible.
//! Мы поднимаем `ICorRuntimeHost`, берём дефолтный AppDomain и дальше работаем
//! late binding'ом через `IDispatch`: `Type` / `FieldInfo` / `MethodInfo` —
//! dual-интерфейсы. Это даёт доступ к полям по именам, без офсетов и
//! паттерн-сканов, и переживает патчи игры.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr;

use windows::Win32::Foundation::VARIANT_BOOL;
use windows::Win32::System::ClrHosting::{
    CLRCreateInstance, CLSID_CLRMetaHost, ICLRMetaHost, ICLRRuntimeInfo, ICorRuntimeHost,
};
use windows::Win32::System::Com::{
    DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPPARAMS, IDispatch, SAFEARRAY,
};
use windows::Win32::System::Ole::{SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_BSTR, VT_DISPATCH, VT_I4, VT_NULL,
    VT_R4, VT_UNKNOWN, VariantClear,
};
use windows::core::{BSTR, GUID, IUnknown, Interface, PCWSTR, PWSTR, Result, w};

/// В биндингах windows-rs этого CLSID нет — задаём вручную.
const CLSID_COR_RUNTIME_HOST: GUID = GUID::from_u128(0xcb2f6723_ab3a_11d2_9c40_00c04fa30a3e);

/// Настоящий `IID_ICorRuntimeHost` из mscoree.h.
///
/// В windows-rs 0.62.2 у `ICorRuntimeHost` проставлен чужой GUID —
/// `84680D3A-B2C1-46E8-ACC2-DBC0A359159A`, то есть `IID_ICorThreadpool`.
/// Раскладка vtable при этом верная, поэтому запрашиваем интерфейс сами
/// с правильным IID, а полученный указатель оборачиваем типом из крейта.
const IID_COR_RUNTIME_HOST: GUID = GUID::from_u128(0xcb2f6722_ab3a_11d2_9c40_00c04fa30a3e);

/// `_AppDomain` объявлен в mscorlib как IDispatch-only интерфейс,
/// поэтому его указатель можно использовать напрямую как `IDispatch`.
const IID_APP_DOMAIN: GUID = GUID::from_u128(0x05f696dc_2b29_3663_ad8b_c4389cf2a713);

const LOCALE_USER_DEFAULT: u32 = 0x0400;

fn err(msg: &str) -> windows::core::Error {
    windows::core::Error::new(windows::Win32::Foundation::E_FAIL, msg)
}

// ---------------------------------------------------------------------------
// VARIANT
// ---------------------------------------------------------------------------

/// Владеющая обёртка над VARIANT: гарантирует VariantClear.
pub struct Var(VARIANT);

impl Drop for Var {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

/// Собирает VARIANT из тега и полезной нагрузки. Поля union'ов
/// присваиваются целиком: писать сквозь `ManuallyDrop` Rust не даёт.
fn build(vt: VARENUM, payload: VARIANT_0_0_0) -> VARIANT {
    let mut v = VARIANT::default();
    v.Anonymous.Anonymous = ManuallyDrop::new(VARIANT_0_0 {
        vt,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: payload,
    });
    v
}

impl Var {
    fn from_raw(v: VARIANT) -> Self {
        Var(v)
    }

    pub fn null() -> Self {
        Var(build(VT_NULL, VARIANT_0_0_0 { llVal: 0 }))
    }

    pub fn int(x: i32) -> Self {
        Var(build(VT_I4, VARIANT_0_0_0 { lVal: x }))
    }

    #[allow(dead_code)]
    pub fn float(x: f32) -> Self {
        Var(build(VT_R4, VARIANT_0_0_0 { fltVal: x }))
    }

    pub fn boolean(x: bool) -> Self {
        let raw = VARIANT_BOOL(if x { -1 } else { 0 });
        Var(build(VT_BOOL, VARIANT_0_0_0 { boolVal: raw }))
    }

    pub fn text(s: &str) -> Self {
        Var(build(
            VT_BSTR,
            VARIANT_0_0_0 {
                bstrVal: ManuallyDrop::new(BSTR::from(s)),
            },
        ))
    }

    pub fn dispatch(d: &IDispatch) -> Self {
        Var(build(
            VT_DISPATCH,
            VARIANT_0_0_0 {
                pdispVal: ManuallyDrop::new(Some(d.clone())),
            },
        ))
    }

    #[allow(dead_code)]
    pub fn vt(&self) -> VARENUM {
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    /// Поверхностная копия для DISPPARAMS. Владение остаётся за `self`,
    /// поэтому копию нельзя дропать — она живёт только на время Invoke.
    fn abi(&self) -> VARIANT {
        unsafe { ptr::read(&self.0) }
    }

    pub fn as_int(&self) -> Option<i32> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt == VT_I4 {
                Some(a.Anonymous.lVal)
            } else if a.vt == VT_R4 {
                Some(a.Anonymous.fltVal as i32)
            } else if a.vt == VT_BOOL {
                Some(if a.Anonymous.boolVal.as_bool() { 1 } else { 0 })
            } else {
                None
            }
        }
    }

    pub fn as_float(&self) -> Option<f32> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt == VT_R4 {
                Some(a.Anonymous.fltVal)
            } else if a.vt == VT_I4 {
                Some(a.Anonymous.lVal as f32)
            } else {
                None
            }
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt == VT_BOOL {
                Some(a.Anonymous.boolVal.as_bool())
            } else if a.vt == VT_I4 {
                Some(a.Anonymous.lVal != 0)
            } else {
                None
            }
        }
    }

    pub fn as_string(&self) -> Option<String> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt != VT_BSTR {
                return None;
            }
            Some(a.Anonymous.bstrVal.to_string())
        }
    }

    /// Достаёт managed-объект. Рефлексия обычно отдаёт VT_DISPATCH,
    /// но элементы массивов иногда приходят как VT_UNKNOWN.
    pub fn as_object(&self) -> Option<IDispatch> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt == VT_DISPATCH {
                return (*a.Anonymous.pdispVal).clone();
            }
            if a.vt == VT_UNKNOWN {
                let unknown = (*a.Anonymous.punkVal).as_ref()?;
                return unknown.cast::<IDispatch>().ok();
            }
            None
        }
    }

    fn as_safearray(&self) -> Option<*mut SAFEARRAY> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt.0 & 0x2000 == 0 {
                return None;
            }
            Some(a.Anonymous.parray)
        }
    }
}

// ---------------------------------------------------------------------------
// Late binding
// ---------------------------------------------------------------------------

fn dispid(target: &IDispatch, name: &str) -> Result<i32> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let names = [PCWSTR(wide.as_ptr())];
    let mut id = 0i32;
    unsafe {
        target.GetIDsOfNames(
            &GUID::zeroed(),
            names.as_ptr(),
            1,
            LOCALE_USER_DEFAULT,
            &mut id,
        )?;
    }
    Ok(id)
}

fn invoke(target: &IDispatch, name: &str, flags: DISPATCH_FLAGS, args: &[Var]) -> Result<Var> {
    let id = dispid(target, name)?;

    // DISPPARAMS ждёт аргументы в обратном порядке. Копии поверхностные,
    // владение осталось у вызывающего, поэтому дропать их нельзя.
    let raw: Vec<VARIANT> = args.iter().rev().map(|v| v.abi()).collect();
    let raw = ManuallyDrop::new(raw);

    let params = DISPPARAMS {
        rgvarg: if raw.is_empty() {
            ptr::null_mut()
        } else {
            raw.as_ptr() as *mut VARIANT
        },
        rgdispidNamedArgs: ptr::null_mut(),
        cArgs: raw.len() as u32,
        cNamedArgs: 0,
    };

    let mut result = VARIANT::default();
    unsafe {
        target.Invoke(
            id,
            &GUID::zeroed(),
            LOCALE_USER_DEFAULT,
            flags,
            &params,
            Some(&mut result),
            None,
            None,
        )?;
    }
    Ok(Var::from_raw(result))
}

/// Вызов метода. У .NET-объектов свойства ходят тем же путём,
/// поэтому флаги объединены.
pub fn call(target: &IDispatch, name: &str, args: &[Var]) -> Result<Var> {
    invoke(
        target,
        name,
        DISPATCH_FLAGS(DISPATCH_METHOD.0 | DISPATCH_PROPERTYGET.0),
        args,
    )
}

// ---------------------------------------------------------------------------
// Рантайм
// ---------------------------------------------------------------------------

pub struct Clr {
    pub domain: IDispatch,
}

impl Clr {
    /// Цепляемся к уже поднятой в процессе CLR.
    ///
    /// `verbose` включает подробный лог по шагам — нужен только на первых
    /// попытках, чтобы не залить лог при ожидании загрузки игры.
    pub fn attach(verbose: bool) -> Result<Self> {
        let meta: ICLRMetaHost = unsafe { CLRCreateInstance(&CLSID_CLRMetaHost) }.map_err(|e| {
            if verbose {
                crate::log!("шаг 1: CLRCreateInstance(CLRMetaHost) не удался: {e}");
            }
            e
        })?;

        let mut runtimes = loaded_runtimes(&meta);
        if runtimes.is_empty() {
            if verbose {
                crate::log!("шаг 2: загруженных рантаймов не видно, пробую v4.0.30319 напрямую");
            }
            if let Ok(info) = unsafe { meta.GetRuntime::<_, ICLRRuntimeInfo>(w!("v4.0.30319")) } {
                runtimes.push(info);
            }
        }
        if runtimes.is_empty() {
            return Err(err("в процессе не найдено ни одного CLR"));
        }

        let mut last = err("не удалось получить дефолтный AppDomain");
        for info in &runtimes {
            let version = runtime_version(info);
            match default_domain(info) {
                Ok(domain) => {
                    if verbose {
                        crate::log!("шаг 3: CLR {version} — дефолтный AppDomain получен");
                    }
                    return Ok(Clr { domain });
                }
                Err(e) => {
                    if verbose {
                        crate::log!("шаг 3: CLR {version} — {e}");
                    }
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// Ищет загруженную сборку по простому имени ("Terraria").
    pub fn assembly(&self, simple_name: &str) -> Result<IDispatch> {
        match self.find_loaded_assembly(simple_name) {
            Ok(found) => Ok(found),
            Err(e) => {
                crate::log!("перебор сборок не дал результата ({e}), пробую AppDomain.Load");
                call(&self.domain, "Load", &[Var::text(simple_name)])?
                    .as_object()
                    .ok_or_else(|| err("AppDomain.Load вернул не объект"))
            }
        }
    }

    fn find_loaded_assembly(&self, simple_name: &str) -> Result<IDispatch> {
        let list = call(&self.domain, "GetAssemblies", &[])?;
        let array = list
            .as_safearray()
            .ok_or_else(|| err("GetAssemblies вернул не массив"))?;

        unsafe {
            let lo = SafeArrayGetLBound(array, 1)?;
            let hi = SafeArrayGetUBound(array, 1)?;
            for i in lo..=hi {
                let mut raw: *mut c_void = ptr::null_mut();
                if SafeArrayGetElement(array, &i, &mut raw as *mut _ as *mut c_void).is_err() {
                    continue;
                }
                if raw.is_null() {
                    continue;
                }
                let unknown = IUnknown::from_raw(raw);
                let Ok(assembly) = unknown.cast::<IDispatch>() else {
                    continue;
                };
                let Ok(full) = call(&assembly, "FullName", &[]) else {
                    continue;
                };
                let Some(full) = full.as_string() else {
                    continue;
                };
                let name = full.split(',').next().unwrap_or("").trim();
                if name.eq_ignore_ascii_case(simple_name) {
                    return Ok(assembly);
                }
            }
        }
        Err(err("сборка не найдена среди загруженных"))
    }
}

fn loaded_runtimes(meta: &ICLRMetaHost) -> Vec<ICLRRuntimeInfo> {
    let mut found = Vec::new();
    unsafe {
        let Ok(enumerator) = meta.EnumerateLoadedRuntimes(GetCurrentProcess()) else {
            return found;
        };
        loop {
            let mut slot: [Option<IUnknown>; 1] = [None];
            let mut fetched = 0u32;
            if enumerator.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(unknown) = slot[0].take() else {
                break;
            };
            if let Ok(info) = unknown.cast::<ICLRRuntimeInfo>() {
                found.push(info);
            }
        }
    }
    found
}

fn runtime_version(info: &ICLRRuntimeInfo) -> String {
    let mut buffer = [0u16; 64];
    let mut len = buffer.len() as u32;
    unsafe {
        if info
            .GetVersionString(Some(PWSTR(buffer.as_mut_ptr())), &mut len)
            .is_ok()
        {
            // len включает завершающий ноль.
            let n = (len as usize).min(buffer.len()).saturating_sub(1);
            return String::from_utf16_lossy(&buffer[..n]);
        }
    }
    "?".to_string()
}

/// Запрашивает `ICorRuntimeHost` в обход сломанного IID в биндингах.
fn cor_runtime_host(info: &ICLRRuntimeInfo) -> Result<ICorRuntimeHost> {
    unsafe {
        let mut raw: *mut c_void = ptr::null_mut();
        let vtable = Interface::vtable(info);
        (vtable.GetInterface)(
            Interface::as_raw(info),
            &CLSID_COR_RUNTIME_HOST,
            &IID_COR_RUNTIME_HOST,
            &mut raw,
        )
        .ok()?;
        if raw.is_null() {
            return Err(err("GetInterface вернул null"));
        }
        Ok(ICorRuntimeHost::from_raw(raw))
    }
}

fn default_domain(info: &ICLRRuntimeInfo) -> Result<IDispatch> {
    let host = cor_runtime_host(info)?;
    let unknown = unsafe { host.GetDefaultDomain() }?;

    if let Ok(dispatch) = unknown.cast::<IDispatch>() {
        return Ok(dispatch);
    }
    // Запасной путь: `_AppDomain` — IDispatch-only интерфейс,
    // его указатель можно использовать как IDispatch напрямую.
    unsafe {
        let mut raw: *mut c_void = ptr::null_mut();
        unknown.query(&IID_APP_DOMAIN, &mut raw).ok()?;
        if raw.is_null() {
            return Err(err("AppDomain не отдал ни IDispatch, ни _AppDomain"));
        }
        Ok(IDispatch::from_raw(raw))
    }
}

// ---------------------------------------------------------------------------
// Типы, поля, массивы
// ---------------------------------------------------------------------------

pub fn get_type(assembly: &IDispatch, full_name: &str) -> Result<IDispatch> {
    call(assembly, "GetType", &[Var::text(full_name)])?
        .as_object()
        .ok_or_else(|| err("Assembly.GetType вернул не тип"))
}

/// Разрешённый один раз `FieldInfo`. Дальше чтение поля — один Invoke.
pub struct Field {
    info: IDispatch,
    #[allow(dead_code)]
    pub name: &'static str,
}

impl Field {
    pub fn resolve(ty: &IDispatch, name: &'static str) -> Result<Field> {
        // Type.GetField(String) по умолчанию ищет Public | Instance | Static —
        // ровно то, что нужно для публичных полей Terraria.
        let info = call(ty, "GetField", &[Var::text(name)])?
            .as_object()
            .ok_or_else(|| err("Type.GetField вернул null"))?;
        Ok(Field { info, name })
    }

    pub fn get_static(&self) -> Result<Var> {
        call(&self.info, "GetValue", &[Var::null()])
    }

    pub fn get(&self, obj: &IDispatch) -> Result<Var> {
        call(&self.info, "GetValue", &[Var::dispatch(obj)])
    }

    pub fn set_static(&self, value: Var) -> Result<()> {
        call(&self.info, "SetValue", &[Var::null(), value])?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn set(&self, obj: &IDispatch, value: Var) -> Result<()> {
        call(&self.info, "SetValue", &[Var::dispatch(obj), value])?;
        Ok(())
    }
}

pub fn array_get(array: &IDispatch, index: i32) -> Result<Var> {
    call(array, "GetValue", &[Var::int(index)])
}

#[allow(dead_code)]
pub fn array_len(array: &IDispatch) -> Result<i32> {
    call(array, "Length", &[])?
        .as_int()
        .ok_or_else(|| err("Array.Length вернул не число"))
}
