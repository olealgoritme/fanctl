//! Lian Li Uni fan hubs (SL, AL, SL v2, AL v2, SL-Infinity) over hidraw. There is no kernel
//! driver; Lian Li's own software is Windows-only, so under Linux the hubs keep whatever they
//! were last told (or follow the motherboard PWM input). Protocol as in liquidctl's lianli_uni
//! driver and EightB1ts/uni-sync.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

const VENDOR: u32 = 0x0cf2;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Sl,
    Al,
    Sli,
    SlV2,
    AlV2,
}

fn kind(pid: u32) -> Option<(Kind, &'static str)> {
    Some(match pid {
        0x7750 | 0xa100 => (Kind::Sl, "Lian Li Uni SL"),
        0xa101 => (Kind::Al, "Lian Li Uni AL"),
        0xa102 => (Kind::Sli, "Lian Li Uni SL-Infinity"),
        0xa103 | 0xa105 => (Kind::SlV2, "Lian Li Uni SL v2"),
        0xa104 => (Kind::AlV2, "Lian Li Uni AL v2"),
        _ => return None,
    })
}

pub struct Hub {
    pub id: String,
    pub name: &'static str,
    pub node: PathBuf,
    kind: Kind,
    /// Channels told to stop following the PWM input, and when: the mode command is sent again
    /// only after 30 s (a USB reset or suspend forgets it), so a duty change is one write.
    fixed: std::cell::Cell<(u8, Option<std::time::Instant>)>,
}

pub const CHANNELS: u8 = 4;

/// `HID_ID=0003:00000CF2:0000A102` in the hidraw device's uevent.
fn hid_id(uevent: &str) -> Option<(u32, u32)> {
    let v = uevent.lines().find_map(|l| l.strip_prefix("HID_ID="))?;
    let mut it = v.split(':').skip(1);
    let vid = u32::from_str_radix(it.next()?, 16).ok()?;
    let pid = u32::from_str_radix(it.next()?, 16).ok()?;
    Some((vid, pid))
}

pub fn hubs() -> Vec<Hub> {
    let mut nodes: Vec<(String, u32)> = fs::read_dir("/sys/class/hidraw")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            let ue = fs::read_to_string(e.path().join("device/uevent")).ok()?;
            let (vid, pid) = hid_id(&ue)?;
            (vid == VENDOR && kind(pid).is_some()).then_some((n, pid))
        })
        .collect();
    nodes.sort_by_key(|(n, _)| n.trim_start_matches("hidraw").parse::<u32>().unwrap_or(0));
    nodes
        .into_iter()
        .enumerate()
        .map(|(i, (n, pid))| {
            let (kind, name) = kind(pid).unwrap();
            Hub {
                id: if i == 0 { "lianli".into() } else { format!("lianli#{}", i + 1) },
                name,
                node: PathBuf::from("/dev").join(n),
                kind,
                fixed: std::cell::Cell::new((0, None)),
            }
        })
        .collect()
}

impl Hub {
    fn pwm_cmd(&self) -> u8 {
        match self.kind {
            Kind::Sl => 49,
            Kind::Al => 66,
            Kind::Sli | Kind::SlV2 | Kind::AlV2 => 98,
        }
    }

    fn open(&self, write: bool) -> std::io::Result<File> {
        OpenOptions::new().read(true).write(write).open(&self.node)
    }

    /// The hub drops a command that follows the previous one too closely; 20 ms is enough
    /// (tested on an SL-Infinity: four channels in 88 ms, every one at its duty).
    fn send(f: &mut File, b: &[u8]) -> std::io::Result<()> {
        f.write_all(b)?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        Ok(())
    }

    /// RPM of all four channels (0 = nothing connected or stopped).
    pub fn rpms(&self) -> Option<[u32; 4]> {
        let f = self.open(false).ok()?;
        let mut buf = [0u8; 65];
        buf[0] = 224;
        // HIDIOCGINPUT(65): _IOC(_IOC_READ | _IOC_WRITE, 'H', 0x0A, 65)
        let req: u64 = (3 << 30) | ((buf.len() as u64) << 16) | ((b'H' as u64) << 8) | 0x0A;
        let r = unsafe { libc::ioctl(f.as_raw_fd(), req as _, buf.as_mut_ptr()) };
        if r < 0 {
            return None;
        }
        let off = match self.kind {
            Kind::SlV2 | Kind::AlV2 => 2,
            _ => 1,
        };
        let mut out = [0u32; 4];
        for (c, o) in out.iter_mut().enumerate() {
            let i = off + c * 2;
            *o = u32::from(buf[i]) << 8 | u32::from(buf[i + 1]);
        }
        Some(out)
    }

    fn speed_byte(&self, pct: f64) -> u8 {
        let p = pct.clamp(0.0, 100.0);
        let b = match self.kind {
            Kind::Sl | Kind::Al if p == 0.0 => 40.0,
            Kind::Sl | Kind::Al => (800.0 + 11.0 * p) / 19.0,
            Kind::Sli if p == 0.0 => 10.0,
            Kind::Sli => (250.0 + 17.5 * p) / 20.0,
            _ if p == 0.0 => 7.0,
            _ => (200.0 + 19.0 * p) / 21.0,
        };
        b as u8
    }

    /// Channel `ch` (1-4) to a fixed duty: stop following the PWM input, then the speed.
    pub fn set(&self, ch: u8, pct: f64) -> std::io::Result<()> {
        let mut f = self.open(true)?;
        let bit = 1u8 << (ch - 1);
        let (mask, at) = self.fixed.get();
        let fresh = at.map_or(false, |t| t.elapsed() < std::time::Duration::from_secs(30));
        if mask & bit == 0 || !fresh {
            Self::send(&mut f, &[224, 16, self.pwm_cmd(), 0x10 << (ch - 1)])?;
            let mask = if fresh { mask | bit } else { bit };
            self.fixed.set((mask, Some(at.filter(|_| fresh).unwrap_or_else(std::time::Instant::now))));
        }
        Self::send(&mut f, &[224, ch + 31, 0, self.speed_byte(pct)])
    }

    /// Channel `ch` follows the hub's motherboard PWM input again.
    pub fn auto(&self, ch: u8) -> std::io::Result<()> {
        let mut f = self.open(true)?;
        let (mask, at) = self.fixed.get();
        self.fixed.set((mask & !(1 << (ch - 1)), at));
        Self::send(&mut f, &[224, 16, self.pwm_cmd(), 0x11 << (ch - 1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hid_id() {
        let ue = "DRIVER=hid-generic\nHID_ID=0003:00000CF2:0000A102\nHID_NAME=ENE LianLi\n";
        assert_eq!(hid_id(ue), Some((0x0cf2, 0xa102)));
        assert_eq!(hid_id("HID_NAME=x\n"), None);
    }

    #[test]
    fn speed_bytes_match_liquidctl() {
        let h = |kind| Hub { id: String::new(), name: "", node: PathBuf::new(), kind, fixed: std::cell::Cell::new((0, None)) };
        assert_eq!(h(Kind::Sli).speed_byte(100.0), 100);
        assert_eq!(h(Kind::Sli).speed_byte(0.0), 10);
        assert_eq!(h(Kind::Sl).speed_byte(100.0), 100);
        assert_eq!(h(Kind::SlV2).speed_byte(100.0), 100);
        assert_eq!(h(Kind::SlV2).speed_byte(150.0), 100);
    }
}
