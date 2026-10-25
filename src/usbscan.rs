//! USB devices from cooling vendors that nothing here controls: no kernel hwmon driver bound,
//! and not one of the hubs fanctl drives itself. Listed so `probe` says what it could not reach.

use std::fs;
use std::path::Path;

const VENDORS: &[(u16, &str)] = &[
    (0x1b1c, "Corsair"),
    (0x1e71, "NZXT"),
    (0x0cf2, "Lian Li (ENE)"),
    (0x0c70, "Aquacomputer"),
    (0x264a, "Thermaltake"),
    (0x2516, "Cooler Master"),
    (0x3633, "DeepCool"),
    (0x1044, "Gigabyte"),
    (0x0b05, "ASUS"),
    (0x1d50, "EK / open hardware"),
    (0x34d3, "Phanteks"),
];

pub struct Unreached {
    pub vendor: &'static str,
    pub vid: u16,
    pub pid: u16,
    pub product: String,
}

fn hex(p: &Path) -> Option<u16> {
    u16::from_str_radix(fs::read_to_string(p).ok()?.trim(), 16).ok()
}

/// Whether any interface of this USB device has a hwmon chip below it.
fn has_hwmon(dev: &Path) -> bool {
    fn walk(p: &Path, depth: u32) -> bool {
        if depth > 6 {
            return false;
        }
        let Ok(rd) = fs::read_dir(p) else { return false };
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name == "hwmon" {
                return true;
            }
            let ft = e.file_type();
            // Interfaces (1-2:1.0) and HID children (0003:1E71:2007.0005), never back up the tree.
            if ft.map(|t| t.is_dir()).unwrap_or(false) && (name.contains(':') || name.contains('.')) && walk(&e.path(), depth + 1) {
                return true;
            }
        }
        false
    }
    walk(dev, 0)
}

/// `handled(vid, pid)`: fanctl drives it itself (the Lian Li hubs).
pub fn unreached(handled: impl Fn(u16, u16) -> bool) -> Vec<Unreached> {
    let mut out = Vec::new();
    for e in fs::read_dir("/sys/bus/usb/devices").into_iter().flatten().flatten() {
        let p = e.path();
        let (Some(vid), Some(pid)) = (hex(&p.join("idVendor")), hex(&p.join("idProduct"))) else { continue };
        let Some(&(_, vendor)) = VENDORS.iter().find(|(v, _)| *v == vid) else { continue };
        let product = fs::read_to_string(p.join("product")).map(|s| s.trim().to_string()).unwrap_or_default();
        // ASUS and Gigabyte make far more than coolers: only their LED and cooler devices count.
        let pl = product.to_lowercase();
        if (vid == 0x0b05 || vid == 0x1044) && !(pl.contains("ryujin") || pl.contains("aio") || pl.contains("waterforce") || pl.contains("aura")) {
            continue;
        }
        if handled(vid, pid) || has_hwmon(&p) {
            continue;
        }
        out.push(Unreached { vendor, vid, pid, product });
    }
    out.sort_by_key(|u| (u.vid, u.pid));
    out
}
