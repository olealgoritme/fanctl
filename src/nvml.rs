//! NVIDIA GPUs through NVML (libnvidia-ml.so.1, loaded at run time so fanctl runs without it).
//! The proprietary driver has no hwmon chip; temperature and fan duty come from here. Setting
//! a fan needs root.

use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};

type Dev = *mut c_void;

pub struct Nvml {
    lib: Library,
}

pub struct Gpu {
    pub id: String,
    pub name: String,
    dev: Dev,
    pub fans: u32,
}

const NVML_SUCCESS: c_int = 0;
const NVML_TEMPERATURE_GPU: c_uint = 0;

impl Nvml {
    pub fn load() -> Option<Nvml> {
        let lib = unsafe { Library::new("libnvidia-ml.so.1") }.ok()?;
        let init: Symbol<unsafe extern "C" fn() -> c_int> = unsafe { lib.get(b"nvmlInit_v2") }.ok()?;
        if unsafe { init() } != NVML_SUCCESS {
            return None;
        }
        drop(init);
        Some(Nvml { lib })
    }

    fn sym<T>(&self, name: &[u8]) -> Option<Symbol<'_, T>> {
        unsafe { self.lib.get(name) }.ok()
    }

    pub fn gpus(&self) -> Vec<Gpu> {
        let mut out = Vec::new();
        let (Some(count), Some(handle)) = (
            self.sym::<unsafe extern "C" fn(*mut c_uint) -> c_int>(b"nvmlDeviceGetCount_v2"),
            self.sym::<unsafe extern "C" fn(c_uint, *mut Dev) -> c_int>(b"nvmlDeviceGetHandleByIndex_v2"),
        ) else {
            return out;
        };
        let name = self.sym::<unsafe extern "C" fn(Dev, *mut c_char, c_uint) -> c_int>(b"nvmlDeviceGetName");
        let nfans = self.sym::<unsafe extern "C" fn(Dev, *mut c_uint) -> c_int>(b"nvmlDeviceGetNumFans");
        let mut n: c_uint = 0;
        if unsafe { count(&mut n) } != NVML_SUCCESS {
            return out;
        }
        for i in 0..n {
            let mut dev: Dev = std::ptr::null_mut();
            if unsafe { handle(i, &mut dev) } != NVML_SUCCESS {
                continue;
            }
            let mut buf = [0 as c_char; 96];
            let gname = match &name {
                Some(f) if unsafe { f(dev, buf.as_mut_ptr(), buf.len() as c_uint) } == NVML_SUCCESS => {
                    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
                }
                _ => "NVIDIA GPU".into(),
            };
            let mut f: c_uint = 0;
            let fans = match &nfans {
                Some(g) if unsafe { g(dev, &mut f) } == NVML_SUCCESS => f,
                _ => 0,
            };
            out.push(Gpu { id: format!("nvidia{i}"), name: gname, dev, fans });
        }
        out
    }

    pub fn temp(&self, g: &Gpu) -> Option<f64> {
        let f = self.sym::<unsafe extern "C" fn(Dev, c_uint, *mut c_uint) -> c_int>(b"nvmlDeviceGetTemperature")?;
        let mut t: c_uint = 0;
        (unsafe { f(g.dev, NVML_TEMPERATURE_GPU, &mut t) } == NVML_SUCCESS).then_some(t as f64)
    }

    pub fn fan_pct(&self, g: &Gpu, fan: u32) -> Option<f64> {
        let f = self.sym::<unsafe extern "C" fn(Dev, c_uint, *mut c_uint) -> c_int>(b"nvmlDeviceGetFanSpeed_v2")?;
        let mut s: c_uint = 0;
        (unsafe { f(g.dev, fan, &mut s) } == NVML_SUCCESS).then_some(s as f64)
    }

    /// RPM of one fan (nvmlDeviceGetFanSpeedRPM, drivers from 535 on).
    pub fn fan_rpm(&self, g: &Gpu, fan: u32) -> Option<f64> {
        #[repr(C)]
        struct Info {
            version: c_uint,
            fan: c_uint,
            speed: c_uint,
        }
        let f = self.sym::<unsafe extern "C" fn(Dev, *mut Info) -> c_int>(b"nvmlDeviceGetFanSpeedRPM")?;
        // NVML_STRUCT_VERSION(FanSpeedInfo, 1)
        let mut i = Info { version: std::mem::size_of::<Info>() as c_uint | 1 << 24, fan, speed: 0 };
        (unsafe { f(g.dev, &mut i) } == NVML_SUCCESS).then_some(i.speed as f64)
    }

    pub fn set_fan(&self, g: &Gpu, fan: u32, pct: f64) -> Result<(), String> {
        let f = self
            .sym::<unsafe extern "C" fn(Dev, c_uint, c_uint) -> c_int>(b"nvmlDeviceSetFanSpeed_v2")
            .ok_or("NVML has no nvmlDeviceSetFanSpeed_v2")?;
        match unsafe { f(g.dev, fan, pct.clamp(0.0, 100.0).round() as c_uint) } {
            NVML_SUCCESS => Ok(()),
            e => Err(format!("NVML error {e} (setting GPU fans needs root)")),
        }
    }

    pub fn auto_fan(&self, g: &Gpu, fan: u32) -> Result<(), String> {
        let f = self
            .sym::<unsafe extern "C" fn(Dev, c_uint) -> c_int>(b"nvmlDeviceSetDefaultFanSpeed_v2")
            .ok_or("NVML has no nvmlDeviceSetDefaultFanSpeed_v2")?;
        match unsafe { f(g.dev, fan) } {
            NVML_SUCCESS => Ok(()),
            e => Err(format!("NVML error {e}")),
        }
    }
}
