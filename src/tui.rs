//! `fanctl` with no arguments on a terminal: temperatures and every connected fan, live, with
//! keys to set duties, hand fans back to their hardware, find a fan, and stop or start the curve
//! daemon.

use super::{Inv, Kind};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Block, BorderType, Borders, Chart, Dataset, GraphType, Paragraph};
use ratatui::{Frame, Terminal};
use std::collections::{HashMap, VecDeque};
use std::process::Command;
use std::time::{Duration, Instant};

const BG: Color = Color::Rgb(13, 15, 20);
const PANEL: Color = Color::Rgb(20, 23, 31);
const EDGE: Color = Color::Rgb(52, 58, 76);
const DIM: Color = Color::Rgb(110, 118, 140);
const FG: Color = Color::Rgb(220, 225, 235);
const GREEN: Color = Color::Rgb(118, 185, 0);
const CYAN: Color = Color::Rgb(0, 200, 220);
const BLUE: Color = Color::Rgb(80, 150, 255);
const VIOLET: Color = Color::Rgb(170, 110, 255);
const AMBER: Color = Color::Rgb(255, 190, 40);
const RED: Color = Color::Rgb(255, 80, 90);
const SEL: Color = Color::Rgb(34, 38, 50);

const HEAT: [Color; 4] = [GREEN, AMBER, Color::Rgb(255, 120, 40), RED];
const COOL: [Color; 3] = [GREEN, CYAN, BLUE];
const HIST: usize = 240;

fn mix(a: Color, b: Color, t: f32) -> Color {
    let c = |c: Color| match c {
        Color::Rgb(r, g, b) => (r as f32, g as f32, b as f32),
        _ => (255.0, 255.0, 255.0),
    };
    let ((ar, ag, ab), (br, bg, bb), t) = (c(a), c(b), t.clamp(0.0, 1.0));
    Color::Rgb((ar + (br - ar) * t) as u8, (ag + (bg - ag) * t) as u8, (ab + (bb - ab) * t) as u8)
}

fn ramp(stops: &[Color], t: f32) -> Color {
    let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (t.floor() as usize).min(stops.len() - 2);
    mix(stops[i], stops[i + 1], t - i as f32)
}

fn panel<'a>(title: &'a str, accent: Color) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(EDGE))
        .style(Style::new().bg(PANEL).fg(FG))
        .title(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(accent)),
            Span::styled(title, Style::new().fg(FG).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
}

/// A smooth bar: eighth blocks, coloured cell by cell along `stops`.
fn bar(width: u16, frac: f64, stops: &[Color]) -> Vec<Span<'static>> {
    const PART: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let w = width as usize;
    let f = frac.clamp(0.0, 1.0) * w as f64;
    let full = f.floor() as usize;
    let rem = ((f - full as f64) * 8.0) as usize;
    (0..w)
        .map(|i| {
            let c = ramp(stops, i as f32 / w.max(1) as f32);
            let s = if i < full {
                "█"
            } else if i == full && rem > 0 {
                PART[rem]
            } else {
                "·"
            };
            let fg = if i < full || (i == full && rem > 0) { c } else { EDGE };
            Span::styled(s, Style::new().fg(fg))
        })
        .collect()
}

/// One summary row: a sensor (or the hottest of a group), and where its bar tops out.
struct Row {
    name: String,
    t: Option<f64>,
    lo: f64,
    hi: f64,
}

struct App {
    inv: Inv,
    sel: usize,
    all: bool,
    rows: Vec<Row>,
    rpms: Vec<Option<[u32; 4]>>,
    cpu: VecDeque<u64>,
    cool: VecDeque<u64>,
    gpu: VecDeque<u64>,
    has_cool: bool,
    has_gpu: bool,
    /// Duties fanctl set from here, for hub channels that do not report theirs.
    set: HashMap<usize, f64>,
    daemon: bool,
    root: bool,
    note: Option<(String, Color, Instant)>,
    identify: Option<(usize, Instant, f64)>,
    /// Duties the service published (super::STATE).
    svc: HashMap<String, f64>,
    tick: u64,
}

fn daemon_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", "fanctl"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

impl App {
    fn label_temp(&self, chip: Option<&str>, label: &str) -> Option<f64> {
        self.inv
            .sensors
            .iter()
            .filter(|s| chip.map_or(true, |c| s.id.split('/').next().map_or(false, |x| x.starts_with(c))))
            .filter(|s| s.label.as_deref().map_or(false, |l| l.eq_ignore_ascii_case(label)))
            .filter_map(|s| self.inv.temp(s))
            .filter(|t| *t > 1.0 && *t < 150.0)
            .fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t))))
    }

    fn chip_max(&self, prefix: &str) -> Option<f64> {
        self.inv
            .sensors
            .iter()
            .filter(|s| s.id.starts_with(prefix))
            .filter_map(|s| self.inv.temp(s))
            .filter(|t| *t > 1.0 && *t < 150.0)
            .fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t))))
    }

    fn refresh(&mut self) {
        self.rpms = self.inv.hub_rpms();
        let gpu = self.inv.sensors.iter().find(|s| s.id.starts_with("nvidia")).and_then(|s| self.inv.temp(s));
        let coolant = self.label_temp(None, "Coolant temp").or_else(|| self.label_temp(None, "Coolant"));
        let mut rows = vec![
            Row { name: "CPU (Tctl)".into(), t: self.label_temp(Some("k10temp"), "Tctl").or_else(|| self.label_temp(Some("coretemp"), "Package id 0")), lo: 30.0, hi: 95.0 },
            Row { name: "CPU CCD1".into(), t: self.label_temp(Some("k10temp"), "Tccd1"), lo: 30.0, hi: 95.0 },
            Row { name: "CPU CCD2".into(), t: self.label_temp(Some("k10temp"), "Tccd2"), lo: 30.0, hi: 95.0 },
            Row { name: "Coolant".into(), t: coolant, lo: 25.0, hi: 50.0 },
            Row { name: "GPU".into(), t: gpu.or_else(|| self.label_temp(Some("amdgpu"), "edge")), lo: 30.0, hi: 90.0 },
            Row { name: "GPU memory".into(), t: self.label_temp(Some("vram"), "VRAM"), lo: 30.0, hi: 100.0 },
            Row { name: "VRM".into(), t: self.label_temp(None, "VRM"), lo: 30.0, hi: 110.0 },
            Row { name: "NVMe (hottest)".into(), t: self.chip_max("nvme"), lo: 25.0, hi: 80.0 },
            Row { name: "RAM (hottest)".into(), t: self.chip_max("spd5118").or_else(|| self.chip_max("jc42")), lo: 25.0, hi: 85.0 },
        ];
        rows.retain(|r| r.t.is_some());
        self.rows = rows;
        let push = |h: &mut VecDeque<u64>, v: Option<f64>| {
            if h.len() == HIST {
                h.pop_front();
            }
            h.push_back(v.unwrap_or(0.0).max(0.0) as u64);
        };
        let cpu = self.rows.first().and_then(|r| r.t);
        let gpu_t = self.rows.iter().find(|r| r.name == "GPU").and_then(|r| r.t);
        self.has_cool = coolant.is_some();
        self.has_gpu = gpu_t.is_some();
        push(&mut self.cpu, cpu);
        push(&mut self.cool, coolant);
        push(&mut self.gpu, gpu_t);
        if self.tick % 4 == 0 {
            self.daemon = daemon_active();
        }
        self.svc = std::fs::read_to_string(super::STATE)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (id, d) = l.rsplit_once(' ')?;
                Some((id.to_string(), d.parse().ok()?))
            })
            .collect();
        // identify: 4 s at 100 %, 4 s at 30 %, then back.
        if let Some((i, t0, back)) = self.identify {
            let s = t0.elapsed().as_secs_f64();
            if s >= 8.0 {
                let _ = self.inv.set(i, back);
                self.set.insert(i, back);
                self.identify = None;
                self.say(format!("{}: back to {back:.0}%", self.inv.controls[i].id), GREEN);
            } else if s >= 4.0 && self.set.get(&i) != Some(&30.0) {
                let _ = self.inv.set(i, 30.0);
                self.set.insert(i, 30.0);
            }
        }
    }

    fn visible(&self) -> Vec<usize> {
        (0..self.inv.controls.len())
            .filter(|&i| self.all || self.inv.rpm(i, &self.rpms) != Some(0.0))
            .collect()
    }

    fn pct(&self, i: usize) -> Option<f64> {
        self.inv.pct(i).or_else(|| self.set.get(&i).copied()).or_else(|| if self.daemon { self.svc.get(&self.inv.controls[i].id).copied() } else { None })
    }

    fn say(&mut self, s: String, c: Color) {
        self.note = Some((s, c, Instant::now()));
    }

    fn may_write(&mut self) -> bool {
        if !self.root {
            self.say("read-only: start fanctl with sudo to change fans".into(), AMBER);
            return false;
        }
        if self.daemon {
            self.say("the fanctl service sets these fans: press d to pause it first".into(), AMBER);
            return false;
        }
        true
    }

    fn set_sel(&mut self, pct: f64) {
        let v = self.visible();
        let Some(&i) = v.get(self.sel) else { return };
        if !self.may_write() {
            return;
        }
        let pct = pct.clamp(0.0, 100.0);
        match self.inv.set(i, pct) {
            Ok(()) => {
                self.set.insert(i, pct);
                self.say(format!("{} → {pct:.0}%", self.inv.controls[i].id), GREEN);
            }
            Err(e) => self.say(format!("{}: {e}", self.inv.controls[i].id), RED),
        }
    }

    fn key(&mut self, code: KeyCode, m: KeyModifiers) -> bool {
        let n = self.visible().len();
        let cur = self.visible().get(self.sel).and_then(|&i| self.pct(i)).unwrap_or(50.0);
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('c') if m.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Left | KeyCode::Char('-') => self.set_sel((cur / 5.0).round() * 5.0 - 5.0),
            KeyCode::Right | KeyCode::Char('+') => self.set_sel((cur / 5.0).round() * 5.0 + 5.0),
            KeyCode::Char('f') => self.set_sel(100.0),
            KeyCode::Char(c @ '1'..='9') => self.set_sel(f64::from(c as u8 - b'0') * 10.0),
            KeyCode::Char('0') => self.set_sel(100.0),
            KeyCode::Char('a') => {
                let v = self.visible();
                if let Some(&i) = v.get(self.sel) {
                    if self.may_write() {
                        match self.inv.auto(i) {
                            Ok(()) => {
                                self.set.remove(&i);
                                self.say(format!("{}: back to its hardware's control", self.inv.controls[i].id), GREEN);
                            }
                            Err(e) => self.say(format!("{}: {e}", self.inv.controls[i].id), RED),
                        }
                    }
                }
            }
            KeyCode::Char('i') => {
                let v = self.visible();
                if let Some(&i) = v.get(self.sel) {
                    if self.identify.is_none() && self.may_write() && self.inv.set(i, 100.0).is_ok() {
                        let back = self.pct(i).unwrap_or(100.0);
                        self.set.insert(i, 100.0);
                        self.identify = Some((i, Instant::now(), back));
                        self.say(format!("{}: 100% then 30% — listen / look", self.inv.controls[i].id), CYAN);
                    }
                }
            }
            KeyCode::Char('d') => {
                if !self.root {
                    self.say("start fanctl with sudo to stop or start the service".into(), AMBER);
                } else {
                    let verb = if self.daemon { "stop" } else { "start" };
                    let ok = Command::new("systemctl").args([verb, "fanctl"]).status().map(|s| s.success()).unwrap_or(false);
                    self.daemon = daemon_active();
                    let msg = match (verb, ok) {
                        ("stop", true) => "service paused: its fans went to 100%, set them here".to_string(),
                        ("start", true) => "service running: the curves set the fans again".to_string(),
                        _ => format!("systemctl {verb} fanctl failed"),
                    };
                    self.say(msg, if ok { GREEN } else { RED });
                }
            }
            KeyCode::Char('h') => {
                self.all = !self.all;
                self.sel = 0;
            }
            _ => {}
        }
        true
    }
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let shift = (app.tick as f32 * 0.05).sin() * 0.5 + 0.5;
    let title = "fanctl";
    let mut l: Vec<Span> = vec![Span::raw(" ")];
    for (i, c) in title.chars().enumerate() {
        let t = (i as f32 / title.len() as f32 + shift * 0.6) % 1.0;
        l.push(Span::styled(c.to_string(), Style::new().fg(ramp(&[GREEN, CYAN, VIOLET, GREEN], t)).bold()));
    }
    let (ds, dc) = if app.daemon { ("● service: curves active", GREEN) } else { ("○ service: off", AMBER) };
    l.push(Span::styled("   ", Style::new()));
    l.push(Span::styled(ds, Style::new().fg(dc)));
    l.push(Span::styled(
        if app.root { "   root: can change fans" } else { "   read-only (sudo fanctl to change)" },
        Style::new().fg(DIM),
    ));
    let counts = format!(
        "   {} controls · {} sensors · {} USB hubs · {} GPUs",
        app.inv.controls.len(),
        app.inv.sensors.len(),
        app.inv.hubs.len(),
        app.inv.gpus.len()
    );
    l.push(Span::styled(counts, Style::new().fg(DIM)));
    // Narrow terminal: keep the title and the service state, drop the rest.
    let mut w = 0usize;
    l.retain(|s| {
        w += s.content.chars().count();
        w <= area.width as usize
    });
    f.render_widget(Paragraph::new(vec![Line::raw(""), Line::from(l)]).style(Style::new().bg(BG)), area);
}

fn temps(f: &mut Frame, app: &App, area: Rect) {
    let block = panel("Temperatures", AMBER);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let bw = inner.width.saturating_sub(26).max(6);
    let lines: Vec<Line> = app
        .rows
        .iter()
        .map(|r| {
            let t = r.t.unwrap_or(0.0);
            let frac = (t - r.lo) / (r.hi - r.lo);
            let c = ramp(&HEAT, frac as f32);
            let mut s = vec![Span::styled(format!(" {:<15}", r.name), Style::new().fg(DIM))];
            s.extend(bar(bw, frac, &HEAT));
            s.push(Span::styled(format!(" {t:>5.1} °C", ), Style::new().fg(c).bold()));
            Line::from(s)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn history(f: &mut Frame, app: &App, area: Rect) {
    // CPU always; next to it the coolant, or the GPU on a machine without a liquid cooler.
    let mut charts = vec![("CPU", &app.cpu, AMBER, 30.0, 100.0)];
    if app.has_cool {
        charts.push(("Coolant", &app.cool, CYAN, 20.0, 50.0));
    } else if app.has_gpu {
        charts.push(("GPU", &app.gpu, VIOLET, 30.0, 90.0));
    }
    let rects = Layout::horizontal(vec![Constraint::Ratio(1, charts.len() as u32); charts.len()]).split(area);
    for ((title, data, accent, lo, hi), &rect) in charts.into_iter().zip(rects.iter()) {
        let last = data.back().copied().unwrap_or(0) as f64;
        let t = format!("{title}  {last:.0} °C");
        let n = data.len();
        // x: seconds before now (one sample a second), y: °C
        let pts: Vec<(f64, f64)> = data.iter().enumerate().map(|(i, v)| (i as f64 - (n as f64 - 1.0), *v as f64)).collect();
        let c = ramp(&HEAT, ((last - lo) / (hi - lo)) as f32);
        let ds = Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(c)).data(&pts);
        let span = HIST as f64 - 1.0;
        let lbl = |s: String| Span::styled(s, Style::new().fg(DIM));
        let chart = Chart::new(vec![ds])
            .block(panel(&t, accent))
            .style(Style::new().bg(PANEL))
            .x_axis(
                Axis::default()
                    .bounds([-span, 0.0])
                    .style(Style::new().fg(EDGE))
                    .labels([lbl(format!("-{}m", (span / 60.0).round())), lbl(format!("-{}m", (span / 120.0).round())), lbl("now".into())]),
            )
            .y_axis(
                Axis::default()
                    .bounds([lo, hi])
                    .style(Style::new().fg(EDGE))
                    .labels([lbl(format!("{lo:.0}°")), lbl(format!("{:.0}°", (lo + hi) / 2.0)), lbl(format!("{hi:.0}°"))]),
            );
        f.render_widget(chart, rect);
    }
}

fn fans(f: &mut Frame, app: &App, area: Rect) {
    let title = if app.all { "Fans and pumps (all channels)" } else { "Fans and pumps" };
    let block = panel(title, CYAN);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let v = app.visible();
    // id 18 + rpm 11 + percent 5 always; the device column (up to 34) and the bar share the rest.
    let rest = (inner.width as usize).saturating_sub(18 + 11 + 5);
    let dw = if rest >= 34 + 12 { 34 } else if rest >= 24 { rest - 12 } else { 0 };
    let bw = (rest - if dw > 0 { dw + 1 } else { 0 }).clamp(4, 30) as u16;
    let mut lines = vec![Line::from(Span::styled(
        format!(" {:<16}{} {:>9}  {:<w$}", "control", if dw > 0 { format!(" {:<dw$}", "device") } else { String::new() }, "rpm", "duty", w = bw as usize),
        Style::new().fg(DIM),
    ))];
    for (row, &i) in v.iter().enumerate() {
        let c = &app.inv.controls[i];
        let rpm = app.inv.rpm(i, &app.rpms);
        let pct = app.pct(i);
        let selected = row == app.sel;
        let bg = if selected { SEL } else { PANEL };
        let marker = if selected { "▸" } else { " " };
        let what: String = c.what.chars().take(dw).collect();
        let icon = match c.kind {
            Kind::Gpu { .. } => "◈",
            Kind::Hub { .. } => "◎",
            Kind::Pwm(_) if c.id.contains("x53") || c.id.contains("kraken") => "◉",
            Kind::Pwm(_) => "○",
        };
        let mut s = vec![
            Span::styled(format!("{marker}{icon} {:<14} ", c.id), Style::new().fg(if selected { CYAN } else { FG }).bg(bg).bold()),
            Span::styled(if dw > 0 { format!("{what:<dw$} ") } else { String::new() }, Style::new().fg(DIM).bg(bg)),
            Span::styled(
                format!("{:>9}  ", rpm.map(|r| format!("{r:.0} rpm")).unwrap_or_else(|| "-".into())),
                Style::new().fg(FG).bg(bg),
            ),
        ];
        s.extend(bar(bw, pct.unwrap_or(0.0) / 100.0, &COOL));
        s.push(Span::styled(
            format!(" {:>4}", pct.map(|p| format!("{p:.0}%")).unwrap_or_else(|| "-".into())),
            Style::new().fg(FG).bg(bg).bold(),
        ));
        if app.identify.map(|(x, _, _)| x) == Some(i) {
            s.push(Span::styled("  ◌ identify", Style::new().fg(CYAN).bg(bg)));
        }
        lines.push(Line::from(s));
    }
    if v.is_empty() {
        lines.push(Line::from(Span::styled(" no fan controls found", Style::new().fg(DIM))));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn footer(f: &mut Frame, app: &App, area: Rect) {
    let [note, keys] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    if let Some((s, c, t)) = &app.note {
        if t.elapsed() < Duration::from_secs(6) {
            f.render_widget(Paragraph::new(Span::styled(format!(" {s}"), Style::new().fg(*c))).style(Style::new().bg(BG)), note);
        }
    }
    let items = [
        ("↑↓", "select"),
        ("←→", "−/+5%"),
        ("1-9", "10-90%"),
        ("f", "100%"),
        ("a", "auto"),
        ("i", "identify"),
        ("d", if app.daemon { "pause service" } else { "start service" }),
        ("h", if app.all { "hide empty" } else { "show all" }),
        ("q", "quit"),
    ];
    let mut spans = Vec::new();
    let mut used = 0u16;
    for (k, label) in items {
        let (ks, ls) = (format!(" {k} "), format!(" {label} "));
        let w = (ks.chars().count() + ls.chars().count() + 1) as u16;
        if used + w > keys.width {
            break;
        }
        used += w;
        spans.push(Span::styled(ks, Style::new().fg(BG).bg(GREEN).bold()));
        spans.push(Span::styled(ls, Style::new().fg(FG).bg(SEL)));
        spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(BG)), keys);
}

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::new().bg(BG)), area);
    let rows = app.rows.len() as u16 + 2;
    let nf = app.visible().len().max(1) as u16 + 3;
    let [head, top, mid, hist, foot] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(rows),
        Constraint::Length(nf.min(area.height.saturating_sub(rows + 12))),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .areas(area);
    header(f, app, head);
    temps(f, app, top);
    fans(f, app, mid);
    history(f, app, hist);
    footer(f, app, foot);
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

pub fn run(inv: Inv) -> std::io::Result<()> {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        prev(info);
    }));
    enable_raw_mode()?;
    let _g = Guard;
    crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let mut app = App {
        inv,
        sel: 0,
        all: false,
        rows: Vec::new(),
        rpms: Vec::new(),
        cpu: VecDeque::with_capacity(HIST),
        cool: VecDeque::with_capacity(HIST),
        gpu: VecDeque::with_capacity(HIST),
        has_cool: false,
        has_gpu: false,
        set: HashMap::new(),
        daemon: daemon_active(),
        root: unsafe { libc::geteuid() } == 0,
        note: None,
        identify: None,
        svc: HashMap::new(),
        tick: 0,
    };
    app.refresh();
    let mut last = Instant::now();
    loop {
        term.draw(|f| draw(f, &app))?;
        app.tick = app.tick.wrapping_add(1);
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press && !app.key(k.code, k.modifiers) {
                    break;
                }
            }
        }
        if last.elapsed() >= Duration::from_secs(1) {
            app.refresh();
            last = Instant::now();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    /// `cargo test -- --ignored --nocapture render`: one frame of this machine, as text.
    #[test]
    #[ignore]
    fn render() {
        let mut app = App {
            inv: Inv::scan_with(true, true),
            sel: 0,
            all: false,
            rows: Vec::new(),
            rpms: Vec::new(),
            cpu: VecDeque::new(),
            cool: VecDeque::new(),
            gpu: VecDeque::new(),
            has_cool: false,
            has_gpu: false,
            set: HashMap::new(),
            daemon: daemon_active(),
            root: false,
            note: None,
            identify: None,
            svc: HashMap::new(),
            tick: 0,
        };
        for _ in 0..30 {
            app.refresh();
        }
        let w: u16 = std::env::var("FANCTL_TEST_W").ok().and_then(|v| v.parse().ok()).unwrap_or(150);
        let mut t = Terminal::new(TestBackend::new(w, 42)).unwrap();
        t.draw(|f| draw(f, &app)).unwrap();
        let b = t.backend().buffer();
        for y in 0..b.area.height {
            let row: String = (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect();
            println!("{}", row.trim_end());
        }
    }
}
