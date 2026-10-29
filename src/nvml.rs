//! Minimo wrapper NVML per gli offset clock NVIDIA.
//!
//! Usa dlopen/dlsym così il demone non dipende da libnvidia-ml a link-time
//! e continua a funzionare su macchine senza GPU NVIDIA.
//!
//! L'API viene inizializzata una sola volta e riusata: chiamare nvmlInit /
//! nvmlShutdown a ogni lettura faceva crescere i file descriptor del demone
//! fino a esaurirli.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::{Mutex, OnceLock};

type NvmlInit = unsafe extern "C" fn() -> c_int;
type NvmlHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> c_int;
type NvmlGetOffset = unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int;
type NvmlSetOffset = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

struct Api {
    _lib: *mut c_void,
    handle: NvmlHandle,
    get_gpc: NvmlGetOffset,
    set_gpc: NvmlSetOffset,
    get_mem: NvmlGetOffset,
    set_mem: NvmlSetOffset,
}

// NVML è thread-safe. Il raw pointer alla libreria viene usato solo tramite
// le funzioni protette dal Mutex globale.
unsafe impl Send for Api {}

static API: OnceLock<Mutex<Option<Api>>> = OnceLock::new();

unsafe fn sym<T: Copy>(lib: *mut c_void, name: &str) -> Option<T> {
    let cname = CString::new(name).ok()?;
    let ptr = libc::dlsym(lib, cname.as_ptr() as *const c_char);
    if ptr.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy::<*mut c_void, T>(&ptr))
    }
}

impl Api {
    fn open() -> Option<Self> {
        unsafe {
            let name = CString::new("libnvidia-ml.so.1").ok()?;
            let lib = libc::dlopen(name.as_ptr() as *const c_char, libc::RTLD_NOW);
            if lib.is_null() {
                return None;
            }
            let init = sym::<NvmlInit>(lib, "nvmlInit_v2")?;
            let handle = sym::<NvmlHandle>(lib, "nvmlDeviceGetHandleByIndex_v2")?;
            let get_gpc = sym::<NvmlGetOffset>(lib, "nvmlDeviceGetGpcClkVfOffset")?;
            let set_gpc = sym::<NvmlSetOffset>(lib, "nvmlDeviceSetGpcClkVfOffset")?;
            let get_mem = sym::<NvmlGetOffset>(lib, "nvmlDeviceGetMemClkVfOffset")?;
            let set_mem = sym::<NvmlSetOffset>(lib, "nvmlDeviceSetMemClkVfOffset")?;

            if init() != 0 {
                libc::dlclose(lib);
                return None;
            }
            Some(Self {
                _lib: lib,
                handle,
                get_gpc,
                set_gpc,
                get_mem,
                set_mem,
            })
        }
    }

    fn device(&self) -> Option<*mut c_void> {
        unsafe {
            let mut dev: *mut c_void = std::ptr::null_mut();
            if (self.handle)(0, &mut dev) != 0 {
                return None;
            }
            Some(dev)
        }
    }
}

fn with_api<T>(f: impl FnOnce(&Api) -> T) -> Option<T> {
    let mutex = API.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().ok()?;
    if guard.is_none() {
        *guard = Api::open();
    }
    guard.as_ref().map(f)
}

fn code_message(code: c_int) -> String {
    match code {
        0 => "ok".to_string(),
        3 => "non supportato dal driver/GPU".to_string(),
        4 => "permesso negato".to_string(),
        6 => "versione driver non compatibile".to_string(),
        _ => format!("errore NVML {code}"),
    }
}

/// Legge gli offset correnti (base clock, memory clock) in MHz.
pub fn get_offsets() -> Option<(i32, i32)> {
    with_api(|api| {
        let dev = api.device()?;
        let mut gpc = 0;
        let mut mem = 0;
        unsafe {
            if (api.get_gpc)(dev, &mut gpc) != 0 {
                return None;
            }
            if (api.get_mem)(dev, &mut mem) != 0 {
                return None;
            }
        }
        Some((gpc, mem))
    })
    .flatten()
}

/// Imposta gli offset (base clock, memory clock) in MHz.
pub fn set_offsets(gpc: i32, mem: i32) -> Result<(), String> {
    let result = with_api(|api| {
        let dev = api
            .device()
            .ok_or_else(|| "GPU NVIDIA non inizializzabile".to_string())?;
        unsafe {
            let ret = (api.set_gpc)(dev, gpc);
            if ret != 0 {
                return Err(format!("base clock offset: {}", code_message(ret)));
            }
            let ret = (api.set_mem)(dev, mem);
            if ret != 0 {
                return Err(format!("memory clock offset: {}", code_message(ret)));
            }
        }
        Ok(())
    });
    match result {
        Some(r) => r,
        None => Err("libnvidia-ml non disponibile".to_string()),
    }
}
