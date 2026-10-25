//! Every chip under /sys/class/hwmon: motherboard Super I/O (nct67xx, it87, ...), CPU and GPU
//! sensors, and the USB coolers and hubs that have kernel drivers (nzxt-kraken2/3, nzxt-smart2,
//! corsair-cpro, corsair-psu, aquacomputer_d5next, gigabyte_waterforce, asus_rog_ryujin, ...).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Chip {
    /// Unique name: the driver's `name`, with `#2`, `#3` ... for repeats (six nvme chips).
    pub id: String,
    pub path: PathBuf,
    /// What the chip sits on (PCI address, USB path, platform device), for the probe listing.
    pub device: String,
}

pub struct Temp {
    pub id: String,
    pub label: Option<String>,
    pub input: PathBuf,
}

pub struct Fan {
    pub id: String,
    pub label: Option<String>,
    pub input: PathBuf,
}

pub struct Pwm {
    pub id: String,
    pub chip: String,
    pub path: PathBuf,
    pub enable: Option<PathBuf>,
    /// The tachometer the kernel numbers like this pwm (fanN for pwmN), when there is one.
    pub fan: Option<PathBuf>,
}

pub fn read_num(p: &Path) -> Option<f64> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn read_str(p: &Path) -> Option<String> {
    let s = fs::read_to_string(p).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn writable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).map(|m| m.permissions().mode() & 0o222 != 0).unwrap_or(false)
}

pub fn chips() -> Vec<Chip> {
    let mut found: Vec<(String, PathBuf, String)> = Vec::new();
    let Ok(dir) = fs::read_dir("/sys/class/hwmon") else { return Vec::new() };
    for e in dir.flatten() {
        let path = e.path();
        let Some(name) = read_str(&path.join("name")) else { continue };
        let device = fs::canonicalize(path.join("device"))
            .map(|d| d.to_string_lossy().trim_start_matches("/sys/devices/").to_string())
            .unwrap_or_else(|_| "virtual".into());
        found.push((name, path, device));
    }
    // hwmonN numbers change between boots; the device path does not.
    found.sort_by(|a, b| (&a.0, &a.2).cmp(&(&b.0, &b.2)));
    let mut seen: HashMap<String, usize> = HashMap::new();
    found
        .into_iter()
        .map(|(name, path, device)| {
            let n = seen.entry(name.clone()).or_insert(0);
            *n += 1;
            let id = if *n == 1 { name } else { format!("{name}#{n}") };
            Chip { id, path, device }
        })
        .collect()
}

/// Indices N of the files `<prefix>N<suffix>` in a chip directory, sorted.
fn indices(dir: &Path, prefix: &str, suffix: &str) -> Vec<u32> {
    let mut v: Vec<u32> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            n.strip_prefix(prefix)?.strip_suffix(suffix)?.parse().ok()
        })
        .collect();
    v.sort_unstable();
    v
}

impl Chip {
    pub fn temps(&self) -> Vec<Temp> {
        indices(&self.path, "temp", "_input")
            .into_iter()
            .map(|i| Temp {
                id: format!("{}/temp{i}", self.id),
                label: read_str(&self.path.join(format!("temp{i}_label"))),
                input: self.path.join(format!("temp{i}_input")),
            })
            .collect()
    }

    pub fn fans(&self) -> Vec<Fan> {
        indices(&self.path, "fan", "_input")
            .into_iter()
            .map(|i| Fan {
                id: format!("{}/fan{i}", self.id),
                label: read_str(&self.path.join(format!("fan{i}_label"))),
                input: self.path.join(format!("fan{i}_input")),
            })
            .collect()
    }

    pub fn pwms(&self) -> Vec<Pwm> {
        indices(&self.path, "pwm", "")
            .into_iter()
            .map(|i| {
                let enable = self.path.join(format!("pwm{i}_enable"));
                let fan = self.path.join(format!("fan{i}_input"));
                Pwm {
                    id: format!("{}/pwm{i}", self.id),
                    chip: self.id.clone(),
                    path: self.path.join(format!("pwm{i}")),
                    enable: enable.exists().then_some(enable),
                    fan: fan.exists().then_some(fan),
                }
            })
            .collect()
    }
}

impl Fan {
    pub fn rpm(&self) -> Option<f64> {
        read_num(&self.input)
    }
}

impl Pwm {
    pub fn writable(&self) -> bool {
        writable(&self.path)
    }

    pub fn pct(&self) -> Option<f64> {
        read_num(&self.path).map(|v| v * 100.0 / 255.0)
    }

    pub fn mode(&self) -> Option<i64> {
        self.enable.as_deref().and_then(read_num).map(|v| v as i64)
    }

    pub fn rpm(&self) -> Option<f64> {
        self.fan.as_deref().and_then(read_num)
    }

    /// Manual mode (pwmN_enable = 1), then the duty.
    pub fn set(&self, pct: f64) -> std::io::Result<()> {
        if let Some(e) = &self.enable {
            if read_num(e) != Some(1.0) {
                fs::write(e, "1")?;
            }
        }
        fs::write(&self.path, format!("{}", (pct.clamp(0.0, 100.0) * 2.55).round() as u32))
    }

    /// Hand the fan back to the chip's own control. Super I/O chips (nct67xx) use 5 for their
    /// SmartFan curve; the generic hwmon value for automatic control is 2.
    pub fn auto(&self) -> std::io::Result<()> {
        let Some(e) = &self.enable else { return Ok(()) };
        let v = if self.chip.starts_with("nct") { "5" } else { "2" };
        fs::write(e, v)
    }
}
