//! NVIDIA VRAM temperature, which NVML does not report. Read-only 32-bit reads of a few
//! memory-controller registers in the GPU's BAR0, through the device's own sysfs file
//! (`/sys/bus/pci/devices/<addr>/resource0`): no `iomem=relaxed`, and nothing outside that
//! GPU's registers can be reached. Needs root. Register recipes from the gddr6 and gpuwatch
//! readers.

use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::path::Path;

enum Recipe {
    /// Ampere/Ada GDDR6/6X: one register, (raw & 0xfff) / 32 C.
    Single(u32),
    /// Blackwell GDDR7: per memory partition, FBPA DQR MR-code in bits 23:16.
    Modules { base: u32, stride: u32, valid: u32, max: u32 },
}

struct Model {
    dev: u16,
    name: &'static str,
    mem: &'static str,
    recipe: Recipe,
}

const fn ada(dev: u16, off: u32, mem: &'static str, name: &'static str) -> Model {
    Model { dev, name, mem, recipe: Recipe::Single(off) }
}

const MODELS: &[Model] = &[
    Model {
        dev: 0x2b85,
        name: "RTX 5090",
        mem: "GDDR7",
        recipe: Recipe::Modules { base: 0x0090_24c0, stride: 0x4000, valid: 0x10, max: 16 },
    },
    ada(0x2684, 0xe2a8, "GDDR6X", "RTX 4090"),
    ada(0x2685, 0xe2a8, "GDDR6X", "RTX 4090 D"),
    ada(0x2702, 0xe2a8, "GDDR6X", "RTX 4080 Super"),
    ada(0x2704, 0xe2a8, "GDDR6X", "RTX 4080"),
    ada(0x2705, 0xe2a8, "GDDR6X", "RTX 4070 Ti Super"),
    ada(0x2782, 0xe2a8, "GDDR6X", "RTX 4070 Ti"),
    ada(0x2783, 0xe2a8, "GDDR6X", "RTX 4070 Super"),
    ada(0x2786, 0xe2a8, "GDDR6X", "RTX 4070"),
    ada(0x2860, 0xe2a8, "GDDR6", "RTX 4070 Mobile"),
    ada(0x28e0, 0xe2a8, "GDDR6", "RTX 4060 Mobile"),
    ada(0x28a0, 0xe2a8, "GDDR6", "RTX 4060 Laptop"),
    ada(0x2203, 0xe2a8, "GDDR6X", "RTX 3090 Ti"),
    ada(0x2204, 0xe2a8, "GDDR6X", "RTX 3090"),
    ada(0x2208, 0xe2a8, "GDDR6X", "RTX 3080 Ti"),
    ada(0x2206, 0xe2a8, "GDDR6X", "RTX 3080"),
    ada(0x2216, 0xe2a8, "GDDR6X", "RTX 3080 LHR"),
    ada(0x2484, 0xee50, "GDDR6", "RTX 3070"),
    ada(0x2488, 0xee50, "GDDR6", "RTX 3070 LHR"),
    ada(0x2531, 0xe2a8, "GDDR6", "RTX A2000"),
    ada(0x2571, 0xe2a8, "GDDR6", "RTX A2000"),
    ada(0x24b0, 0xe2a8, "GDDR6", "RTX A4000"),
    ada(0x2232, 0xe2a8, "GDDR6", "RTX A4500"),
    ada(0x2231, 0xe2a8, "GDDR6", "RTX A5000"),
    ada(0x26b1, 0xe2a8, "GDDR6", "RTX 6000 Ada"),
    ada(0x27b8, 0xe2a8, "GDDR6", "L4"),
    ada(0x26b9, 0xe2a8, "GDDR6", "L40S"),
    ada(0x2236, 0xe2a8, "GDDR6", "A10"),
];

pub struct Gpu {
    pub id: String,
    pub name: String,
    model: &'static Model,
    map: *const u8,
    len: usize,
    _file: File,
}

fn hex(p: &Path) -> Option<u32> {
    let s = fs::read_to_string(p).ok()?;
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// Every supported NVIDIA GPU whose BAR0 this process may map (root).
pub fn gpus() -> Vec<Gpu> {
    let mut devs: Vec<_> = fs::read_dir("/sys/bus/pci/devices").into_iter().flatten().flatten().map(|e| e.path()).collect();
    devs.sort();
    let mut out = Vec::new();
    for d in devs {
        if hex(&d.join("vendor")) != Some(0x10de) {
            continue;
        }
        // VGA (0x0300xx) or 3D controller (0x0302xx); skips the board's audio function.
        let class = hex(&d.join("class")).unwrap_or(0) >> 8;
        if class != 0x0300 && class != 0x0302 {
            continue;
        }
        let Some(dev) = hex(&d.join("device")) else { continue };
        let Some(model) = MODELS.iter().find(|m| u32::from(m.dev) == dev) else { continue };
        let Ok(file) = File::open(d.join("resource0")) else { continue };
        let len = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
        if len == 0 {
            continue;
        }
        let map = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, file.as_raw_fd(), 0) };
        if map == libc::MAP_FAILED {
            continue;
        }
        let n = out.len();
        out.push(Gpu {
            id: if n == 0 { "vram".into() } else { format!("vram#{}", n + 1) },
            name: format!("{} {}", model.name, model.mem),
            model,
            map: map as *const u8,
            len,
            _file: file,
        });
    }
    out
}

fn mrcode(raw: u32) -> f64 {
    let c = ((raw >> 16) & 0xff).min(80) as i32;
    f64::from(if c > 19 { (c - 20) * 2 } else { -(40 - c * 2) })
}

fn plausible(t: f64) -> bool {
    t > -40.0 && t < 130.0
}

impl Gpu {
    fn reg(&self, off: u32) -> Option<u32> {
        let off = off as usize;
        if off % 4 != 0 || off + 4 > self.len {
            return None;
        }
        Some(unsafe { std::ptr::read_volatile(self.map.add(off) as *const u32) })
    }

    /// (module, temperature) of every memory module that reports one; module 0 only on
    /// single-register GPUs.
    pub fn modules(&self) -> Vec<(u32, f64)> {
        match self.model.recipe {
            Recipe::Single(off) => self.reg(off).map(|r| (0, f64::from(r & 0xfff) / 32.0)).filter(|(_, t)| plausible(*t)).into_iter().collect(),
            Recipe::Modules { base, stride, valid, max } => (0..max)
                .filter_map(|p| {
                    let off = base + p * stride;
                    let v = self.reg(off + valid)?;
                    let dq = self.reg(off)?;
                    // All four IC/sub-partition readings valid, and not the 0xbadf... poison value.
                    if (v >> 24) & 0xf != 0xf || dq & 0xffff_0000 == 0xbadf_0000 {
                        return None;
                    }
                    Some((p, mrcode(dq))).filter(|(_, t)| plausible(*t))
                })
                .collect(),
        }
    }

    pub fn hottest(&self) -> Option<f64> {
        self.modules().into_iter().map(|(_, t)| t).reduce(f64::max)
    }

    pub fn module(&self, m: u32) -> Option<f64> {
        self.modules().into_iter().find(|(p, _)| *p == m).map(|(_, t)| t)
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map as *mut libc::c_void, self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mrcode_decodes_like_gddr6() {
        assert_eq!(mrcode(52 << 16), 64.0);
        assert_eq!(mrcode(20 << 16), 0.0);
        assert_eq!(mrcode(10 << 16), -20.0);
        assert_eq!(mrcode(200 << 16), 120.0);
    }
}
