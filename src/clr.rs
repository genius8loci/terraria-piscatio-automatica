//! Доступ к состоянию игры через рефлексию .NET прямо из натива.
//!
//! Terraria — managed-процесс, поэтому CLR уже поднята. Мы поднимаем
//! `ICorRuntimeHost`, берём дефолтный AppDomain и дальше идём по цепочке
//! рефлексии `_AppDomain` -> `_Assembly` -> `_Type` -> `_FieldInfo`.
//!
//! Late binding через `IDispatch` здесь неприменим, и это проверено на живой
//! CLR: объект хостинга AppDomain не отдаёт `IDispatch` вовсе, а у `_AppDomain`
//! слоты IDispatch — заглушки с `E_NOTIMPL`. `System.Type` (RuntimeType) тоже
//! не поддерживает `IDispatch`. Работает только типизированный vtable, поэтому
//! методы вызываются по номерам слотов, снятым из `mscorlib.tlb`.
//!
//! Поля при этом по-прежнему адресуются **по именам**, так что патч игры
//! ломает код только при переименовании полей.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr;

use windows::Win32::Foundation::VARIANT_BOOL;
use windows::Win32::System::ClrHosting::{
    CLRCreateInstance, CLSID_CLRMetaHost, ICLRMetaHost, ICLRRuntimeInfo, ICorRuntimeHost,
};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{
    SafeArrayCreateVector, SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound,
    SafeArrayGetUBound, SafeArrayPutElement,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_ARRAY, VT_BOOL, VT_BSTR, VT_DISPATCH, VT_I4,
    VT_INT, VT_INT_PTR, VT_NULL, VT_R4, VT_UINT, VT_UINT_PTR, VT_UNKNOWN, VT_VARIANT, VariantClear,
};
use windows::core::{BSTR, GUID, HRESULT, IUnknown, Interface, PWSTR, Result, w};

/// В биндингах windows-rs этого CLSID нет — задаём вручную.
const CLSID_COR_RUNTIME_HOST: GUID = GUID::from_u128(0xcb2f6723_ab3a_11d2_9c40_00c04fa30a3e);

/// Настоящий `IID_ICorRuntimeHost` из mscoree.h.
///
/// В windows-rs 0.62.2 у `ICorRuntimeHost` проставлен чужой GUID —
/// `84680D3A-B2C1-46E8-ACC2-DBC0A359159A`, то есть `IID_ICorThreadpool`.
/// Раскладка vtable при этом верная, поэтому интерфейс запрашивается вручную
/// с правильным IID, а указатель оборачивается типом из крейта.
const IID_COR_RUNTIME_HOST: GUID = GUID::from_u128(0xcb2f6722_ab3a_11d2_9c40_00c04fa30a3e);

const IID_APP_DOMAIN: GUID = GUID::from_u128(0x05f696dc_2b29_3663_ad8b_c4389cf2a713);
/// `_Type` из mscorlib.tlb — нужен, когда тип получен как обычный объект.
const IID_TYPE: GUID = GUID::from_u128(0xbca8b44d_aad6_3a86_8ab7_03349f4f2da2);

/// BindingFlags из System.Reflection.
pub const BINDING_INSTANCE: i32 = 4;
pub const BINDING_STATIC: i32 = 8;
pub const BINDING_NON_PUBLIC: i32 = 32;

// Номера слотов vtable, снятые из mscorlib.tlb
// (Windows\Microsoft.NET\Framework\v4.0.30319\mscorlib.tlb).
// Первые 7 слотов у всех этих интерфейсов — IUnknown + IDispatch.
const SLOT_APPDOMAIN_LOAD_2: usize = 44; // Load_2(BSTR, _Assembly**)
const SLOT_APPDOMAIN_GET_ASSEMBLIES: usize = 57; // GetAssemblies(SAFEARRAY**)
const SLOT_ASSEMBLY_GET_FULLNAME: usize = 15; // get_FullName(BSTR*)
const SLOT_ASSEMBLY_GETTYPE_2: usize = 17; // GetType_2(BSTR, _Type**)
const SLOT_TYPE_GETFIELD: usize = 47; // GetField(BSTR, BindingFlags, _FieldInfo**)
const SLOT_TYPE_GETMETHOD_6: usize = 66; // GetMethod_6(BSTR, _MethodInfo**)
const SLOT_TYPE_GETFIELD_2: usize = 68; // GetField_2(BSTR, _FieldInfo**)
const SLOT_FIELDINFO_GETVALUE: usize = 19; // GetValue(VARIANT, VARIANT*)
const SLOT_FIELDINFO_SETVALUE_2: usize = 25; // SetValue_2(VARIANT, VARIANT)
const SLOT_METHODINFO_INVOKE_3: usize = 37; // Invoke_3(VARIANT, SAFEARRAY*, VARIANT*)

type FnOutPtr = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
type FnBstrOutPtr = unsafe extern "system" fn(*mut c_void, *mut u16, *mut *mut c_void) -> HRESULT;
type FnOutBstr = unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT;
type FnVariantOutVariant = unsafe extern "system" fn(*mut c_void, VARIANT, *mut VARIANT) -> HRESULT;
type FnVariantVariant = unsafe extern "system" fn(*mut c_void, VARIANT, VARIANT) -> HRESULT;
type FnInvoke3 =
    unsafe extern "system" fn(*mut c_void, VARIANT, *mut SAFEARRAY, *mut VARIANT) -> HRESULT;
type FnBstrFlagsOutPtr =
    unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut *mut c_void) -> HRESULT;

pub fn err(msg: &str) -> windows::core::Error {
    windows::core::Error::new(windows::Win32::Foundation::E_FAIL, msg)
}

/// Достаёт функцию из vtable объекта по номеру слота.
unsafe fn vfn<T: Copy>(obj: &IUnknown, index: usize) -> T {
    unsafe {
        let this = Interface::as_raw(obj);
        let vtable = *(this as *const *const *const c_void);
        let entry = *vtable.add(index);
        *(&entry as *const *const c_void as *const T)
    }
}

fn this(obj: &IUnknown) -> *mut c_void {
    Interface::as_raw(obj)
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

    /// Managed-объект внутри VARIANT.
    pub fn as_unknown(&self) -> Option<IUnknown> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt == VT_UNKNOWN {
                return (*a.Anonymous.punkVal).clone();
            }
            if a.vt == VT_DISPATCH {
                let dispatch = (*a.Anonymous.pdispVal).as_ref()?;
                return dispatch.cast::<IUnknown>().ok();
            }
            None
        }
    }

    /// Приводит значение к `_Type`. Нужно, когда тип пришёл как обычный
    /// объект — например из `Object.GetType()`.
    pub fn as_type(&self) -> Option<Type> {
        let unknown = self.as_unknown()?;
        unsafe {
            let mut out: *mut c_void = ptr::null_mut();
            unknown.query(&IID_TYPE, &mut out).ok().ok()?;
            if out.is_null() {
                return None;
            }
            Some(Type(IUnknown::from_raw(out)))
        }
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
        let value = VARIANT_BOOL(if x { -1 } else { 0 });
        Var(build(VT_BOOL, VARIANT_0_0_0 { boolVal: value }))
    }

    #[allow(dead_code)]
    pub fn text(s: &str) -> Self {
        Var(build(
            VT_BSTR,
            VARIANT_0_0_0 {
                bstrVal: ManuallyDrop::new(BSTR::from(s)),
            },
        ))
    }

    /// Оборачивает объект, добавляя ссылку.
    #[allow(dead_code)]
    pub fn object(unknown: &IUnknown) -> Self {
        Var::owned_object(unknown.clone())
    }

    /// Забирает владение ссылкой, без дополнительного AddRef.
    fn owned_object(unknown: IUnknown) -> Self {
        Var(build(
            VT_UNKNOWN,
            VARIANT_0_0_0 {
                punkVal: ManuallyDrop::new(Some(unknown)),
            },
        ))
    }

    pub fn vt(&self) -> VARENUM {
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    /// Поверхностная копия для передачи аргументом. Владение остаётся
    /// за `self`, поэтому копию дропать нельзя.
    fn abi(&self) -> VARIANT {
        unsafe { ptr::read(&self.0) }
    }

    pub fn is_null(&self) -> bool {
        let vt = self.vt();
        vt == VT_NULL || vt.0 == 0
    }

    /// Нативный указатель из managed-значения.
    ///
    /// `IntPtr` и результат `RuntimeMethodHandle.GetFunctionPointer()`
    /// приезжают как **`VT_INT` (22 = 0x16)**, а не `VT_INT_PTR` (37):
    /// проверено на живой CLR. Остальные целочисленные теги принимаем
    /// на случай других сборок рантайма.
    pub fn as_ptr(&self) -> Option<usize> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt == VT_INT
                || a.vt == VT_UINT
                || a.vt == VT_INT_PTR
                || a.vt == VT_UINT_PTR
                || a.vt == VT_I4
            {
                return Some(a.Anonymous.lVal as usize);
            }
            None
        }
    }

    pub fn as_int(&self) -> Option<i32> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt == VT_I4 || a.vt == VT_INT || a.vt == VT_UINT || a.vt == VT_INT_PTR {
                Some(a.Anonymous.lVal)
            } else if a.vt == VT_R4 {
                Some(a.Anonymous.fltVal as i32)
            } else if a.vt == VT_BOOL {
                Some(i32::from(a.Anonymous.boolVal.as_bool()))
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

    fn safearray(&self) -> Option<*mut SAFEARRAY> {
        unsafe {
            let a = &self.0.Anonymous.Anonymous;
            if a.vt.0 & VT_ARRAY.0 == 0 {
                return None;
            }
            let array = a.Anonymous.parray;
            if array.is_null() { None } else { Some(array) }
        }
    }
}

// ---------------------------------------------------------------------------
// Массивы
// ---------------------------------------------------------------------------

/// Длина managed-массива, приехавшего как SAFEARRAY.
#[allow(dead_code)]
pub fn array_len(array: &Var) -> Result<i32> {
    let handle = array
        .safearray()
        .ok_or_else(|| err("значение не является managed-массивом"))?;
    unsafe {
        let lo = SafeArrayGetLBound(handle, 1)?;
        let hi = SafeArrayGetUBound(handle, 1)?;
        Ok(hi - lo + 1)
    }
}

/// Элемент managed-массива по индексу.
///
/// Managed-массивы маршалятся в SAFEARRAY — проверено на живой CLR:
/// `Type.EmptyTypes` приезжает как `VT_ARRAY | VT_UNKNOWN`. Поэтому
/// рефлексия для индексации не нужна.
pub fn array_get(array: &Var, index: i32) -> Result<Var> {
    let handle = array
        .safearray()
        .ok_or_else(|| err("значение не является managed-массивом"))?;
    let element = VARENUM(array.vt().0 & 0x0FFF);

    unsafe {
        let lo = SafeArrayGetLBound(handle, 1)?;
        let at = lo + index;

        if element == VT_UNKNOWN || element == VT_DISPATCH {
            let mut slot: *mut c_void = ptr::null_mut();
            SafeArrayGetElement(handle, &at, &mut slot as *mut _ as *mut c_void)?;
            if slot.is_null() {
                return Ok(Var::null());
            }
            return Ok(Var::owned_object(IUnknown::from_raw(slot)));
        }
        if element == VT_VARIANT {
            let mut slot = VARIANT::default();
            SafeArrayGetElement(handle, &at, &mut slot as *mut _ as *mut c_void)?;
            return Ok(Var::from_raw(slot));
        }
        if element == VT_R4 {
            let mut slot = 0f32;
            SafeArrayGetElement(handle, &at, &mut slot as *mut _ as *mut c_void)?;
            return Ok(Var::float(slot));
        }
        if element == VT_I4 {
            let mut slot = 0i32;
            SafeArrayGetElement(handle, &at, &mut slot as *mut _ as *mut c_void)?;
            return Ok(Var::int(slot));
        }
        if element == VT_BOOL {
            let mut slot = VARIANT_BOOL(0);
            SafeArrayGetElement(handle, &at, &mut slot as *mut _ as *mut c_void)?;
            return Ok(Var::boolean(slot.as_bool()));
        }
        if element == VT_BSTR {
            let mut slot: *mut u16 = ptr::null_mut();
            SafeArrayGetElement(handle, &at, &mut slot as *mut _ as *mut c_void)?;
            if slot.is_null() {
                return Ok(Var::null());
            }
            return Ok(Var(build(
                VT_BSTR,
                VARIANT_0_0_0 {
                    bstrVal: ManuallyDrop::new(BSTR::from_raw(slot)),
                },
            )));
        }
    }
    Err(err("неподдерживаемый тип элемента массива"))
}

// ---------------------------------------------------------------------------
// Рефлексия
// ---------------------------------------------------------------------------

pub struct Assembly(IUnknown);
pub struct Type(IUnknown);
pub struct Method(IUnknown);

pub struct Field {
    info: IUnknown,
    #[allow(dead_code)]
    pub name: &'static str,
}

impl Assembly {
    pub fn full_name(&self) -> Result<String> {
        unsafe {
            let f: FnOutBstr = vfn(&self.0, SLOT_ASSEMBLY_GET_FULLNAME);
            let mut out: *mut u16 = ptr::null_mut();
            f(this(&self.0), &mut out).ok()?;
            if out.is_null() {
                return Ok(String::new());
            }
            Ok(BSTR::from_raw(out).to_string())
        }
    }

    pub fn get_type(&self, full_name: &str) -> Result<Type> {
        unsafe {
            let f: FnBstrOutPtr = vfn(&self.0, SLOT_ASSEMBLY_GETTYPE_2);
            let name = BSTR::from(full_name);
            let mut out: *mut c_void = ptr::null_mut();
            f(this(&self.0), name.as_ptr() as *mut u16, &mut out).ok()?;
            if out.is_null() {
                return Err(err("тип не найден в сборке"));
            }
            Ok(Type(IUnknown::from_raw(out)))
        }
    }
}

impl Type {
    /// `Type.GetField(String)` — поиск по умолчанию идёт по
    /// Public | Instance | Static, ровно как нужно для полей Terraria.
    pub fn field(&self, name: &'static str) -> Result<Field> {
        unsafe {
            let f: FnBstrOutPtr = vfn(&self.0, SLOT_TYPE_GETFIELD_2);
            let bstr = BSTR::from(name);
            let mut out: *mut c_void = ptr::null_mut();
            f(this(&self.0), bstr.as_ptr() as *mut u16, &mut out).ok()?;
            if out.is_null() {
                return Err(err("поле не найдено"));
            }
            Ok(Field {
                info: IUnknown::from_raw(out),
                name,
            })
        }
    }

    /// `Type.GetField(String, BindingFlags)` — для непубличных полей,
    /// до которых обычный `GetField(String)` не достаёт.
    pub fn field_flags(&self, name: &'static str, flags: i32) -> Result<Field> {
        unsafe {
            let f: FnBstrFlagsOutPtr = vfn(&self.0, SLOT_TYPE_GETFIELD);
            let bstr = BSTR::from(name);
            let mut out: *mut c_void = ptr::null_mut();
            f(this(&self.0), bstr.as_ptr() as *mut u16, flags, &mut out).ok()?;
            if out.is_null() {
                return Err(err("поле не найдено"));
            }
            Ok(Field {
                info: IUnknown::from_raw(out),
                name,
            })
        }
    }

    /// `Type.GetMethod(String)`. На перегруженных именах бросает
    /// AmbiguousMatchException — годится только для уникальных методов.
    pub fn method(&self, name: &str) -> Result<Method> {
        unsafe {
            let f: FnBstrOutPtr = vfn(&self.0, SLOT_TYPE_GETMETHOD_6);
            let bstr = BSTR::from(name);
            let mut out: *mut c_void = ptr::null_mut();
            f(this(&self.0), bstr.as_ptr() as *mut u16, &mut out).ok()?;
            if out.is_null() {
                return Err(err("метод не найден"));
            }
            Ok(Method(IUnknown::from_raw(out)))
        }
    }
}

impl Field {
    pub fn get(&self, target: &Var) -> Result<Var> {
        unsafe {
            let f: FnVariantOutVariant = vfn(&self.info, SLOT_FIELDINFO_GETVALUE);
            let mut out = VARIANT::default();
            f(this(&self.info), target.abi(), &mut out).ok()?;
            Ok(Var::from_raw(out))
        }
    }

    pub fn set(&self, target: &Var, value: Var) -> Result<()> {
        unsafe {
            let f: FnVariantVariant = vfn(&self.info, SLOT_FIELDINFO_SETVALUE_2);
            f(this(&self.info), target.abi(), value.abi()).ok()?;
        }
        Ok(())
    }

    pub fn get_static(&self) -> Result<Var> {
        self.get(&Var::null())
    }

    pub fn set_static(&self, value: Var) -> Result<()> {
        self.set(&Var::null(), value)
    }
}

impl Method {
    /// Сам `MethodInfo` как значение — чтобы вызывать методы на нём самом
    /// (например `get_MethodHandle`).
    #[allow(dead_code)]
    pub fn as_var(&self) -> Var {
        Var::object(&self.0)
    }

    /// `MethodInfo.Invoke(object, object[])`.
    pub fn invoke(&self, target: &Var, args: &[Var]) -> Result<Var> {
        unsafe {
            let params = if args.is_empty() {
                ptr::null_mut()
            } else {
                let array = SafeArrayCreateVector(VT_VARIANT, 0, args.len() as u32);
                if array.is_null() {
                    return Err(err("не удалось создать SAFEARRAY аргументов"));
                }
                for (i, arg) in args.iter().enumerate() {
                    let at = i as i32;
                    let value = arg.abi();
                    if SafeArrayPutElement(array, &at, &value as *const _ as *const c_void).is_err()
                    {
                        let _ = SafeArrayDestroy(array);
                        return Err(err("не удалось положить аргумент в SAFEARRAY"));
                    }
                }
                array
            };

            let f: FnInvoke3 = vfn(&self.0, SLOT_METHODINFO_INVOKE_3);
            let mut out = VARIANT::default();
            let hr = f(this(&self.0), target.abi(), params, &mut out);

            if !params.is_null() {
                let _ = SafeArrayDestroy(params);
            }
            hr.ok()?;
            Ok(Var::from_raw(out))
        }
    }
}

// ---------------------------------------------------------------------------
// Рантайм
// ---------------------------------------------------------------------------

pub struct Clr {
    domain: IUnknown,
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
    pub fn assembly(&self, simple_name: &str, verbose: bool) -> Result<Assembly> {
        match self.find_loaded_assembly(simple_name) {
            Ok(found) => Ok(found),
            Err(e) => {
                if verbose {
                    crate::log!("шаг 4: перебор сборок не дал результата ({e}), пробую Load");
                }
                self.load_assembly(simple_name)
            }
        }
    }

    fn load_assembly(&self, simple_name: &str) -> Result<Assembly> {
        unsafe {
            let f: FnBstrOutPtr = vfn(&self.domain, SLOT_APPDOMAIN_LOAD_2);
            let name = BSTR::from(simple_name);
            let mut out: *mut c_void = ptr::null_mut();
            f(this(&self.domain), name.as_ptr() as *mut u16, &mut out).ok()?;
            if out.is_null() {
                return Err(err("AppDomain.Load вернул null"));
            }
            Ok(Assembly(IUnknown::from_raw(out)))
        }
    }

    fn find_loaded_assembly(&self, simple_name: &str) -> Result<Assembly> {
        let list = unsafe {
            let f: FnOutPtr = vfn(&self.domain, SLOT_APPDOMAIN_GET_ASSEMBLIES);
            let mut out: *mut c_void = ptr::null_mut();
            f(this(&self.domain), &mut out).ok()?;
            if out.is_null() {
                return Err(err("GetAssemblies вернул null"));
            }
            out as *mut SAFEARRAY
        };

        let found = unsafe {
            let lo = SafeArrayGetLBound(list, 1)?;
            let hi = SafeArrayGetUBound(list, 1)?;
            let mut hit = None;
            for i in lo..=hi {
                let mut slot: *mut c_void = ptr::null_mut();
                if SafeArrayGetElement(list, &i, &mut slot as *mut _ as *mut c_void).is_err()
                    || slot.is_null()
                {
                    continue;
                }
                let assembly = Assembly(IUnknown::from_raw(slot));
                let Ok(full) = assembly.full_name() else {
                    continue;
                };
                let name = full.split(',').next().unwrap_or("").trim();
                if name.eq_ignore_ascii_case(simple_name) {
                    hit = Some(assembly);
                    break;
                }
            }
            let _ = SafeArrayDestroy(list);
            hit
        };

        found.ok_or_else(|| err("сборка не найдена среди загруженных"))
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
        let mut out: *mut c_void = ptr::null_mut();
        let vtable = Interface::vtable(info);
        (vtable.GetInterface)(
            Interface::as_raw(info),
            &CLSID_COR_RUNTIME_HOST,
            &IID_COR_RUNTIME_HOST,
            &mut out,
        )
        .ok()?;
        if out.is_null() {
            return Err(err("GetInterface вернул null"));
        }
        Ok(ICorRuntimeHost::from_raw(out))
    }
}

/// Дефолтный AppDomain как `_AppDomain`.
///
/// Прямой QI на `IDispatch` здесь всегда отдаёт `E_NOINTERFACE`: объект
/// хостинга late binding не поддерживает, поэтому сразу берём типизированный
/// интерфейс.
fn default_domain(info: &ICLRRuntimeInfo) -> Result<IUnknown> {
    let host = cor_runtime_host(info)?;
    // В игре рантайм уже запущен и вызов вернёт S_FALSE, но если DLL попала
    // в процесс до старта CLR, без этого GetDefaultDomain даёт E_UNEXPECTED.
    let _ = unsafe { host.Start() };
    let unknown = unsafe { host.GetDefaultDomain() }?;

    unsafe {
        let mut out: *mut c_void = ptr::null_mut();
        unknown.query(&IID_APP_DOMAIN, &mut out).ok()?;
        if out.is_null() {
            return Err(err("AppDomain не отдал интерфейс _AppDomain"));
        }
        Ok(IUnknown::from_raw(out))
    }
}
