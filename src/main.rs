mod config;
mod hwmon;
mod lianli;
mod nvml;
mod tui;
mod usbscan;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const USAGE: &str = "fanctl: find and control every fan, pump and temperature sensor

usage:
  fanctl                       live view (on a terminal; `sudo fanctl` to change fans)
  fanctl probe                 everything found: controllers, fans, pumps, sensors
  fanctl status                temperatures and every fan control, one line each
  fanctl set <control> <pct>   fixed duty (control id, 'chip/*' or 'all')
  fanctl auto <control>        give the control back to its hardware
  fanctl identify <control>    100 % then 30 % for a few seconds each, to find which fan it is
  fanctl run [config]          curve daemon (default /etc/fanctl.toml)

Control ids look like nct6799/pwm2, x53/pwm1, lianli/fan3, nvidia0/fan0.
Sensor ids look like k10temp/temp1 or k10temp/Tctl (chip/label).
Writing needs root.";

enum Kind {
    Pwm(hwmon::Pwm),
    Hub { hub: usize, ch: u8 },
    Gpu { gpu: usize, fan: u32 },
}

struct Control {
    id: String,
    what: String,
    kind: Kind,
}

enum Src {
    File(std::path::PathBuf),
    Gpu(usize),
}

struct Sensor {
    id: String,
    label: Option<String>,
    src: Src,
}

struct Inv {
    chips: Vec<hwmon::Chip>,
    hubs: Vec<lianli::Hub>,
    nvml: Option<nvml::Nvml>,
    gpus: Vec<nvml::Gpu>,
    controls: Vec<Control>,
    sensors: Vec<Sensor>,
}

impl Inv {
    fn scan() -> Inv {
        let chips = hwmon::chips();
        let hubs = lianli::hubs();
        let nvml = nvml::Nvml::load();
        let gpus = nvml.as_ref().map(|n| n.gpus()).unwrap_or_default();
        let mut controls = Vec::new();
        let mut sensors = Vec::new();
        for c in &chips {
            for t in c.temps() {
                sensors.push(Sensor { id: t.id.clone(), label: t.label.clone(), src: Src::File(t.input) });
            }
            for p in c.pwms() {
                let what = format!("{} pwm", c.id);
                controls.push(Control { id: p.id.clone(), what, kind: Kind::Pwm(p) });
            }
        }
        for (i, h) in hubs.iter().enumerate() {
            for ch in 1..=lianli::CHANNELS {
                controls.push(Control {
                    id: format!("{}/fan{ch}", h.id),
                    what: format!("{} channel {ch}", h.name),
                    kind: Kind::Hub { hub: i, ch },
                });
            }
        }
        for (i, g) in gpus.iter().enumerate() {
            sensors.push(Sensor { id: format!("{}/temp", g.id), label: Some("GPU".into()), src: Src::Gpu(i) });
            for f in 0..g.fans {
                controls.push(Control {
                    id: format!("{}/fan{f}", g.id),
                    what: format!("{} fan {f}", g.name),
                    kind: Kind::Gpu { gpu: i, fan: f },
                });
            }
        }
        Inv { chips, hubs, nvml, gpus, controls, sensors }
    }

    fn temp(&self, s: &Sensor) -> Option<f64> {
        match &s.src {
            Src::File(p) => hwmon::read_num(p).map(|m| m / 1000.0),
            Src::Gpu(i) => self.nvml.as_ref()?.temp(&self.gpus[*i]),
        }
    }

    /// `chip/tempN`, or `chip/<label>` (any case).
    fn sensor(&self, spec: &str) -> Option<&Sensor> {
        self.sensors.iter().find(|s| s.id == spec).or_else(|| {
            let (chip, label) = spec.split_once('/')?;
            self.sensors.iter().find(|s| {
                s.id.split_once('/').map(|(c, _)| c) == Some(chip)
                    && s.label.as_deref().map_or(false, |l| l.eq_ignore_ascii_case(label))
            })
        })
    }

    fn select(&self, spec: &str) -> Vec<usize> {
        let all = || 0..self.controls.len();
        if spec == "all" {
            return all().filter(|&i| self.writable(i)).collect();
        }
        if let Some(prefix) = spec.strip_suffix("/*") {
            return all()
                .filter(|&i| self.controls[i].id.split_once('/').map(|(c, _)| c) == Some(prefix) && self.writable(i))
                .collect();
        }
        all().filter(|&i| self.controls[i].id == spec).collect()
    }

    fn writable(&self, i: usize) -> bool {
        match &self.controls[i].kind {
            Kind::Pwm(p) => p.writable(),
            _ => true,
        }
    }

    fn pct(&self, i: usize) -> Option<f64> {
        match &self.controls[i].kind {
            Kind::Pwm(p) => p.pct(),
            Kind::Hub { .. } => None, // the hubs do not report the duty back
            Kind::Gpu { gpu, fan } => self.nvml.as_ref()?.fan_pct(&self.gpus[*gpu], *fan),
        }
    }

    fn rpm(&self, i: usize, hub_rpms: &[Option<[u32; 4]>]) -> Option<f64> {
        match &self.controls[i].kind {
            Kind::Pwm(p) => p.rpm(),
            Kind::Hub { hub, ch } => hub_rpms[*hub].map(|r| r[(*ch - 1) as usize] as f64),
            Kind::Gpu { gpu, fan } => self.nvml.as_ref()?.fan_rpm(&self.gpus[*gpu], *fan),
        }
    }

    fn mode(&self, i: usize) -> String {
        match &self.controls[i].kind {
            Kind::Pwm(p) => match p.mode() {
                Some(0) => "full".into(),
                Some(1) => "manual".into(),
                Some(m) => format!("auto({m})"),
                None => "-".into(),
            },
            _ => "-".into(),
        }
    }

    fn set(&self, i: usize, pct: f64) -> Result<(), String> {
        match &self.controls[i].kind {
            Kind::Pwm(p) => p.set(pct).map_err(|e| e.to_string()),
            Kind::Hub { hub, ch } => self.hubs[*hub].set(*ch, pct).map_err(|e| e.to_string()),
            Kind::Gpu { gpu, fan } => self.nvml.as_ref().ok_or("no NVML")?.set_fan(&self.gpus[*gpu], *fan, pct),
        }
    }

    fn auto(&self, i: usize) -> Result<(), String> {
        match &self.controls[i].kind {
            Kind::Pwm(p) => p.auto().map_err(|e| e.to_string()),
            Kind::Hub { hub, ch } => self.hubs[*hub].auto(*ch).map_err(|e| e.to_string()),
            Kind::Gpu { gpu, fan } => self.nvml.as_ref().ok_or("no NVML")?.auto_fan(&self.gpus[*gpu], *fan),
        }
    }

    fn hub_rpms(&self) -> Vec<Option<[u32; 4]>> {
        self.hubs.iter().map(|h| h.rpms()).collect()
    }
}

fn f1(v: Option<f64>, unit: &str) -> String {
    v.map(|x| format!("{x:.1}{unit}")).unwrap_or_else(|| "-".into())
}
fn f0(v: Option<f64>, unit: &str) -> String {
    v.map(|x| format!("{x:.0}{unit}")).unwrap_or_else(|| "-".into())
}

fn probe(inv: &Inv) {
    let rpms = inv.hub_rpms();
    println!("== hwmon chips");
    for c in &inv.chips {
        let (t, f, p) = (c.temps().len(), c.fans().len(), c.pwms().len());
        println!("  {:<18} {} temps, {} fans, {} pwm   {}", c.id, t, f, p, c.device);
    }
    println!("\n== USB fan hubs driven by fanctl");
    if inv.hubs.is_empty() {
        println!("  none");
    }
    for (i, h) in inv.hubs.iter().enumerate() {
        let r = rpms[i].map(|r| format!("{r:?} rpm")).unwrap_or_else(|| "rpm unreadable (root?)".into());
        println!("  {:<18} {} on {}   {}", h.id, h.name, h.node.display(), r);
    }
    println!("\n== GPUs (NVML)");
    match &inv.nvml {
        None => println!("  NVML not available"),
        Some(_) if inv.gpus.is_empty() => println!("  none"),
        Some(_) => inv.gpus.iter().for_each(|g| println!("  {:<18} {}, {} fans", g.id, g.name, g.fans)),
    }
    let unreached = usbscan::unreached(|v, p| v == 0x0cf2 && inv.hubs.iter().any(|_| (0xa100..=0xa105).contains(&p) || p == 0x7750));
    println!("\n== USB devices from cooling vendors nothing here controls");
    if unreached.is_empty() {
        println!("  none");
    }
    for u in unreached {
        println!("  {:04x}:{:04x} {} {}  (no fan driver here; may be LED only, or a liquidctl device)", u.vid, u.pid, u.vendor, u.product);
    }
    println!("\n== temperatures");
    for s in &inv.sensors {
        println!("  {:<22} {:<18} {}", s.id, s.label.as_deref().unwrap_or(""), f1(inv.temp(s), " C"));
    }
    println!("\n== fan controls");
    for i in 0..inv.controls.len() {
        let c = &inv.controls[i];
        let rw = if inv.writable(i) { "" } else { "  (read-only for this user)" };
        println!(
            "  {:<18} {:>8} {:>6}  {:<9} {}{}",
            c.id,
            f0(inv.rpm(i, &rpms), " rpm"),
            f0(inv.pct(i), "%"),
            inv.mode(i),
            c.what,
            rw
        );
    }
    println!("\n== tachometers without a control");
    for c in &inv.chips {
        let pwms: Vec<String> = c.pwms().iter().filter_map(|p| p.fan.clone()).map(|p| p.display().to_string()).collect();
        for f in c.fans() {
            if !pwms.contains(&f.input.display().to_string()) {
                println!("  {:<18} {:>8}  {}", f.id, f0(f.rpm(), " rpm"), f.label.as_deref().unwrap_or(""));
            }
        }
    }
}

fn status(inv: &Inv) {
    let rpms = inv.hub_rpms();
    let temps: Vec<String> = inv
        .sensors
        .iter()
        .filter(|s| s.label.is_some() || matches!(s.src, Src::Gpu(_)))
        .filter_map(|s| inv.temp(s).filter(|t| *t > 1.0 && *t < 150.0).map(|t| format!("{}={t:.1}", s.label.as_deref().unwrap_or(&s.id))))
        .collect();
    println!("{}", temps.join("  "));
    for i in 0..inv.controls.len() {
        let rpm = inv.rpm(i, &rpms);
        // An unconnected header or hub channel: no point listing it.
        if matches!(inv.controls[i].kind, Kind::Pwm(_) | Kind::Hub { .. }) && rpm == Some(0.0) {
            continue;
        }
        println!("{:<18} {:>8} {:>6}  {}", inv.controls[i].id, f0(rpm, " rpm"), f0(inv.pct(i), "%"), inv.controls[i].what);
    }
}

fn pick(inv: &Inv, spec: &str) -> Vec<usize> {
    let v = inv.select(spec);
    if v.is_empty() {
        eprintln!("fanctl: no control '{spec}' (see `fanctl probe`)");
        std::process::exit(2);
    }
    v
}

fn apply(inv: &Inv, ids: &[usize], f: impl Fn(usize) -> Result<(), String>) -> i32 {
    let mut rc = 0;
    for &i in ids {
        match f(i) {
            Ok(()) => println!("{}: ok", inv.controls[i].id),
            Err(e) => {
                eprintln!("{}: {e}", inv.controls[i].id);
                rc = 1;
            }
        }
    }
    rc
}

fn identify(inv: &Inv, i: usize) -> i32 {
    let before = inv.pct(i);
    for (pct, secs) in [(100.0, 5), (30.0, 5)] {
        if let Err(e) = inv.set(i, pct) {
            eprintln!("{}: {e}", inv.controls[i].id);
            return 1;
        }
        println!("{} at {pct:.0}% for {secs} s (listen / look)", inv.controls[i].id);
        for _ in 0..secs {
            std::thread::sleep(Duration::from_secs(1));
            let rpms = inv.hub_rpms();
            println!("    {}", f0(inv.rpm(i, &rpms), " rpm"));
        }
    }
    // Never leave a fan at the low step: back to what it read before, or full.
    let back = before.unwrap_or(100.0);
    let _ = inv.set(i, back);
    println!("{} back to {back:.0}%", inv.controls[i].id);
    0
}

/// Where `fanctl run` says which duty it gave each control: the live view shows it for hub
/// channels, which do not report theirs.
pub const STATE: &str = "/run/fanctl.duty";

fn publish(inv: &Inv, curves: &[(&Vec<usize>, f64)]) {
    let mut s = String::new();
    for (ids, duty) in curves {
        for &i in ids.iter() {
            s += &format!("{} {duty:.1}\n", inv.controls[i].id);
        }
    }
    let tmp = format!("{STATE}.tmp");
    if std::fs::write(&tmp, s).is_ok() {
        let _ = std::fs::rename(&tmp, STATE);
    }
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn run(inv: &Inv, path: &str) -> i32 {
    let cfg = match config::Config::load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fanctl: {e}");
            return 2;
        }
    };
    for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(s, on_signal as libc::sighandler_t) };
    }
    // Resolve every curve once; a missing control or sensor is reported, not fatal.
    struct Live<'a> {
        curve: &'a config::Curve,
        controls: Vec<usize>,
        sensors: Vec<&'a Sensor>,
        duty: f64,
    }
    let mut live: Vec<Live> = Vec::new();
    for c in &cfg.curve {
        let controls: Vec<usize> = c.controls.iter().flat_map(|s| inv.select(s)).collect();
        let sensors: Vec<&Sensor> = c.sensors.iter().filter_map(|s| inv.sensor(s)).collect();
        for s in &c.sensors {
            if inv.sensor(s).is_none() {
                eprintln!("fanctl: curve '{}': no sensor '{s}'", c.name);
            }
        }
        for s in &c.controls {
            if inv.select(s).is_empty() {
                eprintln!("fanctl: curve '{}': no control '{s}'", c.name);
            }
        }
        println!("curve '{}': {} controls, {} sensors", c.name, controls.len(), sensors.len());
        live.push(Live { curve: c, controls, sensors, duty: cfg.fail_pct });
    }
    let crit: Vec<(&Sensor, f64)> = cfg
        .critical
        .iter()
        .filter_map(|c| match inv.sensor(&c.sensor) {
            Some(s) => Some((s, c.above)),
            None => {
                eprintln!("fanctl: critical: no sensor '{}'", c.sensor);
                None
            }
        })
        .collect();
    let tick = Duration::from_millis(cfg.interval_ms.max(200));
    let mut last_write = Instant::now() - Duration::from_secs(3600);
    let mut written: Vec<Option<f64>> = vec![None; live.len()];
    let mut was_critical = false;
    let mut prev = Instant::now();
    while !STOP.load(Ordering::SeqCst) {
        let dt = prev.elapsed().as_secs_f64();
        prev = Instant::now();
        let critical = crit.iter().any(|(s, above)| inv.temp(s).map_or(true, |t| t > *above));
        if critical != was_critical {
            println!("critical {}", if critical { "on: everything to 100%" } else { "off" });
            was_critical = critical;
        }
        // Hubs forget a manual duty on a USB reset or after suspend: say it again every 30 s.
        let refresh = last_write.elapsed() > Duration::from_secs(30);
        for (k, l) in live.iter_mut().enumerate() {
            let t = l.sensors.iter().map(|s| inv.temp(s)).try_fold(f64::MIN, |m, t| t.map(|t| m.max(t)));
            let target = match t {
                Some(t) if !l.sensors.is_empty() => l.curve.duty(t),
                _ => cfg.fail_pct,
            };
            l.duty = if target >= l.duty { target } else { (l.duty - l.curve.down_per_s * dt).max(target) };
            let want = if critical { 100.0 } else { l.duty };
            let changed = written[k].map_or(true, |w| (w - want).abs() >= 1.0);
            if changed || refresh {
                for &i in &l.controls {
                    if let Err(e) = inv.set(i, want) {
                        eprintln!("{}: {e}", inv.controls[i].id);
                    }
                }
                written[k] = Some(want);
            }
        }
        publish(inv, &live.iter().map(|l| (&l.controls, if was_critical { 100.0 } else { l.duty })).collect::<Vec<_>>());
        if refresh {
            last_write = Instant::now();
        }
        std::thread::sleep(tick);
    }
    let _ = std::fs::remove_file(STATE);
    println!("stopping: curve controls to {:.0}%", cfg.fail_pct);
    for l in &live {
        for &i in &l.controls {
            let _ = inv.set(i, cfg.fail_pct);
        }
    }
    0
}

fn main() {
    // `fanctl probe | head`: end quietly on a closed pipe instead of panicking.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let inv = Inv::scan();
    let rc = match a.as_slice() {
        ["probe"] => {
            probe(&inv);
            0
        }
        [] if unsafe { libc::isatty(1) } == 1 => match tui::run(inv) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("fanctl: {e}");
                1
            }
        },
        ["status"] | [] => {
            status(&inv);
            0
        }
        ["set", spec, pct] => match pct.trim_end_matches('%').parse::<f64>() {
            Ok(p) => apply(&inv, &pick(&inv, spec), |i| inv.set(i, p)),
            Err(_) => {
                eprintln!("fanctl: '{pct}' is not a percentage");
                2
            }
        },
        ["auto", spec] => apply(&inv, &pick(&inv, spec), |i| inv.auto(i)),
        ["identify", spec] => {
            let v = pick(&inv, spec);
            v.iter().map(|&i| identify(&inv, i)).max().unwrap_or(0)
        }
        ["run"] => run(&inv, "/etc/fanctl.toml"),
        ["run", path] => run(&inv, path),
        _ => {
            println!("{USAGE}");
            if matches!(a.as_slice(), ["-h"] | ["--help"] | ["help"]) { 0 } else { 2 }
        }
    };
    std::process::exit(rc);
}
