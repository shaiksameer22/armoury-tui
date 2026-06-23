//! The reactive ratatui render loop (Module 3 in the Python original).
//!
//! A tokio event loop `select!`s over a periodic tick and the crossterm event
//! stream. Each tick collects a `Snapshot` off-thread (`spawn_blocking`, so the
//! UI never blocks on NVML / D-Bus / sysfs), updates rolling history buffers and
//! redraws. Phase A renders the Dashboard, Power (read-only) and Network tabs;
//! controls and the process table arrive in Phase B / C.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::config::Config;
use crate::control::{ControlResult, Controller, FanCurve};
use crate::render::{self, amber, cyan, dim, magenta, neon, red, text};
use crate::scanner::HardwareMap;
use crate::telemetry::{kill_process, NetConn, ProcDetail, ProcInfo, Snapshot, Telemetry};

const HIST: usize = 60; // samples kept for sparklines / trend graphs
const PROC_ROWS: usize = 40; // max rows shown in the process table

const TABS: [&str; 5] = [
    "⬢ Dashboard",
    "⚡ Power / Fans",
    "✦ Lighting",
    "⇅ Network",
    "☰ Processes",
];
const PROC_SORTS: [&str; 4] = ["cpu", "mem", "pid", "name"];

/// A click action bound to an on-screen region (filled in each `draw`).
#[derive(Clone)]
enum Act {
    Tab(usize),
    Profile(String),
    Charge(i64),
    Brightness(i64),
    AuraCycle,
    AuraColor((u8, u8, u8)),
    AuraSpeed,
    AuraDir,
    SwitchCurveProfile,
    CurveAdjust(i32),
    CurveEnable(bool),
    CurveReset,
    ProcSort(&'static str),
    ProcSelect(u32),
    Kill(bool),
    ConfirmKill(bool), // true = go ahead, false = cancel
    CycleTheme,
    FullCharge,
    AutoSwitch(bool), // on_ac
    CycleEpp,
    Preset(String),
}

struct App {
    hw: HardwareMap,
    tel: Arc<Mutex<Telemetry>>,
    control: Arc<Mutex<Controller>>,
    control_available: bool,
    profiles: Vec<String>,
    curve_profile: String,
    curves: Vec<FanCurve>,
    aura_mode: Option<u32>,
    aura_supported: Vec<u32>,
    aura_c1: (u8, u8, u8),
    aura_c2: (u8, u8, u8),
    aura_speed: String,
    aura_dir: String,
    auto: Option<(u32, u32, bool, bool)>, // profile on_ac/on_bat, auto-switch ac/bat
    epps: Option<(u32, u32, u32)>,        // balanced/performance/quiet EPP
    log: Option<std::fs::File>,
    tab: usize,
    latest: Option<Snapshot>,
    toast: Option<(String, Instant, Color)>,
    alert_cpu: bool,
    alert_gpu: bool,

    // Mouse hit-regions, rebuilt every frame (draw is &self → interior mut).
    zones: RefCell<Vec<(Rect, Act)>>,
    // Process tab (Phase C).
    proc_sort: &'static str,
    proc_filter: String,
    filtering: bool,
    selected_pid: Option<u32>,
    detail: Option<ProcDetail>,
    confirm: Option<(u32, String, bool)>, // pid, name, force — kill dialog
    connections: Vec<NetConn>,
    help: bool,
    batt_alerted: bool,
    fan_alerted: bool,
    cfg: Config,
    applied_rule: Option<usize>,
    notif: Option<zbus::blocking::Connection>, // session bus for desktop notifications

    cpu_hist: VecDeque<f64>,
    gpu_hist: VecDeque<f64>,
    cputemp_hist: VecDeque<f64>,
    gputemp_hist: VecDeque<f64>,
    fan_hist: HashMap<String, VecDeque<f64>>,
    net_down_hist: VecDeque<f64>,
    net_up_hist: VecDeque<f64>,
}

impl App {
    fn new(hw: HardwareMap, log_path: Option<String>) -> Self {
        let tel = Telemetry::new(hw.clone());
        let control = Controller::new(&hw);
        let control_available = control.available();
        let profiles = control.list_profiles();
        // Default the fan-curve editor to whichever profile is enabled, else Performance.
        let curve_profile = "Performance".to_string();
        let log = log_path.and_then(|p| open_log(&p));
        let cfg = Config::load();
        crate::theme::set_by_label(&cfg.theme);
        let tab = cfg.startup_tab.min(TABS.len() - 1);
        App {
            hw,
            tel: Arc::new(Mutex::new(tel)),
            control: Arc::new(Mutex::new(control)),
            control_available,
            profiles,
            curve_profile,
            curves: Vec::new(),
            aura_mode: None,
            aura_supported: Vec::new(),
            aura_c1: (0xff, 0x2e, 0x88),
            aura_c2: (0x36, 0xf9, 0xf6),
            aura_speed: "Med".into(),
            aura_dir: "Right".into(),
            auto: None,
            epps: None,
            log,
            tab,
            latest: None,
            toast: None,
            alert_cpu: false,
            alert_gpu: false,
            zones: RefCell::new(Vec::new()),
            proc_sort: "cpu",
            proc_filter: String::new(),
            filtering: false,
            selected_pid: None,
            detail: None,
            confirm: None,
            connections: Vec::new(),
            help: false,
            batt_alerted: false,
            fan_alerted: false,
            cfg,
            applied_rule: None,
            notif: zbus::blocking::Connection::session().ok(),
            cpu_hist: VecDeque::with_capacity(HIST),
            gpu_hist: VecDeque::with_capacity(HIST),
            cputemp_hist: VecDeque::with_capacity(HIST),
            gputemp_hist: VecDeque::with_capacity(HIST),
            fan_hist: HashMap::new(),
            net_down_hist: VecDeque::with_capacity(HIST),
            net_up_hist: VecDeque::with_capacity(HIST),
        }
    }

    /// Collect a snapshot off-thread, then fold it into history + alerts + log.
    async fn collect(&mut self) {
        let tel = Arc::clone(&self.tel);
        let snap = match tokio::task::spawn_blocking(move || tel.lock().unwrap_or_else(|e| e.into_inner()).snapshot()).await {
            Ok(s) => s,
            Err(_) => return,
        };
        self.push_history(&snap);
        self.check_alerts(&snap);
        self.eval_rules(&snap).await;
        self.write_log(&snap);
        self.latest = Some(snap);
        // Connections enumeration scans /proc — only do it while the tab is open.
        if self.tab == 3 {
            let tel = Arc::clone(&self.tel);
            if let Ok(c) =
                tokio::task::spawn_blocking(move || tel.lock().unwrap_or_else(|e| e.into_inner()).connections(60)).await
            {
                self.connections = c;
            }
        }
    }

    // -- controls (Phase B) -----------------------------------------------

    /// Run a controller action off-thread, toast the result, then reconcile.
    ///
    /// The select loop handles one key event at a time and awaits the action to
    /// completion before the next, so presses can't stack — no explicit lock
    /// needed (asusd D-Bus calls are fast). Reconcile reads true state back:
    /// profile / charge / brightness ride in on the telemetry snapshot, fan
    /// curves are re-read from the daemon.
    async fn run_ctl<F>(&mut self, busy: impl Into<String>, f: F)
    where
        F: FnOnce(&Controller) -> ControlResult + Send + 'static,
    {
        self.set_toast(busy.into(), amber());
        let ctl = Arc::clone(&self.control);
        match tokio::task::spawn_blocking(move || f(&ctl.lock().unwrap_or_else(|e| e.into_inner()))).await {
            Ok(r) => self.set_toast(r.message, if r.ok { neon() } else { red() }),
            Err(_) => self.set_toast("control task failed".into(), red()),
        }
        self.collect().await;
        self.reload_curves().await;
    }

    /// Re-read daemon state that isn't in the telemetry snapshot: fan curves for
    /// the selected profile, and the current/supported Aura modes.
    async fn reload_curves(&mut self) {
        let ctl = Arc::clone(&self.control);
        let prof = self.curve_profile.clone();
        let res = tokio::task::spawn_blocking(move || {
            let c = ctl.lock().unwrap_or_else(|e| e.into_inner());
            (
                c.get_fan_curves(&prof),
                c.current_aura_mode(),
                c.supported_aura_modes(),
                c.auto_profiles(),
                c.epps(),
            )
        })
        .await;
        if let Ok((curves, mode, supported, auto, epps)) = res {
            self.curves = curves;
            self.aura_mode = mode;
            self.aura_supported = supported;
            self.auto = auto;
            self.epps = epps;
        }
    }

    fn current_profile(&self) -> Option<String> {
        self.latest.as_ref().and_then(|s| s.profile.clone())
    }

    async fn cycle_profile(&mut self) {
        if self.profiles.is_empty() {
            return;
        }
        let cur = self.current_profile();
        let idx = self.profiles.iter().position(|n| Some(n) == cur.as_ref());
        let next = idx.map(|i| (i + 1) % self.profiles.len()).unwrap_or(0);
        let name = self.profiles[next].clone();
        self.run_ctl(format!("→ {name} profile"), move |c| c.set_profile(&name))
            .await;
    }

    async fn adjust_charge(&mut self, delta: i64) {
        let cur = self
            .latest
            .as_ref()
            .and_then(|s| s.battery.charge_limit)
            .unwrap_or(80);
        let nv = (cur + delta).clamp(20, 100);
        self.run_ctl(format!("→ charge limit {nv}%"), move |c| {
            c.set_charge_limit(nv)
        })
        .await;
    }

    async fn adjust_brightness(&mut self, delta: i64) {
        let cur = self
            .latest
            .as_ref()
            .and_then(|s| s.kbd_brightness)
            .unwrap_or(0);
        let nv = (cur + delta).clamp(0, 3);
        self.run_ctl("→ brightness", move |c| c.set_brightness(nv))
            .await;
    }

    async fn cycle_aura(&mut self) {
        self.run_ctl("→ aura mode", |c| c.cycle_aura_mode()).await;
    }

    /// Apply the current effect with the editor's colour/speed/direction.
    async fn apply_aura(&mut self) {
        let mode = self.aura_mode.unwrap_or(0);
        let (c1, c2) = (self.aura_c1, self.aura_c2);
        let (sp, dr) = (self.aura_speed.clone(), self.aura_dir.clone());
        self.run_ctl("aura…", move |c| c.set_aura_full(mode, c1, c2, &sp, &dr))
            .await;
    }

    async fn curve_enable(&mut self, enabled: bool) {
        let prof = self.curve_profile.clone();
        let verb = if enabled { "enabling" } else { "disabling" };
        self.run_ctl(format!("{verb} {prof} curves…"), move |c| {
            c.set_curve_enabled(&prof, enabled)
        })
        .await;
    }

    async fn curve_reset(&mut self) {
        let prof = self.curve_profile.clone();
        self.run_ctl(format!("resetting {prof} curve…"), move |c| {
            c.reset_fan_curve(&prof)
        })
        .await;
    }

    fn switch_curve_profile(&mut self) {
        if self.profiles.is_empty() {
            return;
        }
        let idx = self
            .profiles
            .iter()
            .position(|n| *n == self.curve_profile)
            .unwrap_or(0);
        self.curve_profile = self.profiles[(idx + 1) % self.profiles.len()].clone();
    }

    /// Shift every duty-cycle point by `delta` %, clamped 0-100, temps fixed,
    /// then enable the curve so the edit takes effect (cooler/quieter nudge).
    async fn curve_adjust(&mut self, delta: i32) {
        if self.curves.is_empty() {
            self.set_toast("no curve loaded to adjust".into(), amber());
            return;
        }
        let prof = self.curve_profile.clone();
        let curves = self.curves.clone();
        let verb = if delta > 0 { "cooler" } else { "quieter" };
        self.set_toast(format!("{prof} curves {verb} ({delta:+}%)"), amber());
        let ctl = Arc::clone(&self.control);
        let res = tokio::task::spawn_blocking(move || {
            let c = ctl.lock().unwrap_or_else(|e| e.into_inner());
            for cur in &curves {
                let mut nc = cur.clone();
                for p in nc.points.iter_mut() {
                    let pct = (p.1 as f64 / 255.0 * 100.0).round() as i32;
                    let np = (pct + delta).clamp(0, 100) as f64 / 100.0 * 255.0;
                    p.1 = np.round() as u8;
                }
                let r = c.set_fan_curve(&prof, &nc);
                if !r.ok {
                    return r;
                }
            }
            c.set_curve_enabled(&prof, true)
        })
        .await;
        match res {
            Ok(r) => self.set_toast(r.message, if r.ok { neon() } else { red() }),
            Err(_) => self.set_toast("curve adjust failed".into(), red()),
        }
        self.reload_curves().await;
    }

    // -- input handling (keyboard + mouse) --------------------------------

    /// Returns `true` if the app should quit.
    async fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Kill-confirm modal swallows all input.
        if let Some((pid, _, force)) = self.confirm.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.do_kill(pid, force).await
                }
                _ => self.confirm = None,
            }
            return false;
        }

        // Filter text entry on the Processes tab.
        if self.filtering {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.proc_filter.pop();
                }
                KeyCode::Char(c) => self.proc_filter.push(c),
                _ => {}
            }
            return false;
        }

        // Any key dismisses the help overlay.
        if self.help {
            self.help = false;
            return false;
        }

        match key.code {
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('r') => self.collect().await,
            KeyCode::Char('t') => self.cycle_theme(),
            KeyCode::Char(c @ '1'..='5') => self.tab = c as usize - '1' as usize,
            code => match self.tab {
                1 => match code {
                    KeyCode::Char('p') => self.cycle_profile().await,
                    KeyCode::Char(']') => self.adjust_charge(5).await,
                    KeyCode::Char('[') => self.adjust_charge(-5).await,
                    KeyCode::Char('s') => {
                        self.switch_curve_profile();
                        self.reload_curves().await;
                    }
                    KeyCode::Char('c') => self.curve_adjust(5).await,
                    KeyCode::Char('v') => self.curve_adjust(-5).await,
                    KeyCode::Char('e') => self.curve_enable(true).await,
                    KeyCode::Char('d') => self.curve_enable(false).await,
                    KeyCode::Char('x') => self.curve_reset().await,
                    KeyCode::Char('f') => self.dispatch(Act::FullCharge).await,
                    KeyCode::Char('a') => self.dispatch(Act::AutoSwitch(true)).await,
                    KeyCode::Char('b') => self.dispatch(Act::AutoSwitch(false)).await,
                    KeyCode::Char('g') => self.dispatch(Act::CycleEpp).await,
                    _ => {}
                },
                2 => match code {
                    KeyCode::Char('+') | KeyCode::Char('=') => self.adjust_brightness(1).await,
                    KeyCode::Char('-') | KeyCode::Char('_') => self.adjust_brightness(-1).await,
                    KeyCode::Char('m') => self.cycle_aura().await,
                    _ => {}
                },
                4 => match code {
                    KeyCode::Char('/') => self.filtering = true,
                    KeyCode::Char('c') => self.proc_sort = "cpu",
                    KeyCode::Char('m') => self.proc_sort = "mem",
                    KeyCode::Char('p') => self.proc_sort = "pid",
                    KeyCode::Char('n') => self.proc_sort = "name",
                    KeyCode::Down => self.move_selection(1).await,
                    KeyCode::Up => self.move_selection(-1).await,
                    KeyCode::Char('k') => self.request_kill(false),
                    KeyCode::Char('K') => self.request_kill(true),
                    _ => {}
                },
                _ => {}
            },
        }
        false
    }

    async fn handle_mouse(&mut self, ev: crossterm::event::MouseEvent) {
        if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        let (col, row) = (ev.column, ev.row);
        // Topmost zone wins → search newest-first.
        let act = self
            .zones
            .borrow()
            .iter()
            .rev()
            .find(|(r, _)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
            .map(|(_, a)| a.clone());
        if let Some(a) = act {
            self.dispatch(a).await;
        }
    }

    async fn dispatch(&mut self, act: Act) {
        match act {
            Act::Tab(i) => self.tab = i,
            Act::Profile(name) => {
                self.run_ctl(format!("→ {name} profile"), move |c| c.set_profile(&name))
                    .await
            }
            Act::Charge(d) => self.adjust_charge(d).await,
            Act::Brightness(d) => self.adjust_brightness(d).await,
            Act::AuraCycle => self.cycle_aura().await,
            Act::AuraColor(rgb) => {
                self.aura_c1 = rgb;
                self.apply_aura().await;
            }
            Act::AuraSpeed => {
                self.aura_speed = match self.aura_speed.as_str() {
                    "Low" => "Med",
                    "Med" => "High",
                    _ => "Low",
                }
                .into();
                self.apply_aura().await;
            }
            Act::AuraDir => {
                self.aura_dir = match self.aura_dir.as_str() {
                    "Up" => "Down",
                    "Down" => "Left",
                    "Left" => "Right",
                    _ => "Up",
                }
                .into();
                self.apply_aura().await;
            }
            Act::SwitchCurveProfile => {
                self.switch_curve_profile();
                self.reload_curves().await;
            }
            Act::CurveAdjust(d) => self.curve_adjust(d).await,
            Act::CurveEnable(b) => self.curve_enable(b).await,
            Act::CurveReset => self.curve_reset().await,
            Act::ProcSort(s) => self.proc_sort = s,
            Act::ProcSelect(pid) => self.select_pid(pid).await,
            Act::Kill(force) => self.request_kill(force),
            Act::ConfirmKill(go) => {
                if go {
                    if let Some((pid, _, force)) = self.confirm.clone() {
                        self.do_kill(pid, force).await;
                    }
                } else {
                    self.confirm = None;
                }
            }
            Act::CycleTheme => self.cycle_theme(),
            Act::FullCharge => {
                self.run_ctl("one-shot full charge…", |c| c.one_shot_full_charge())
                    .await
            }
            Act::AutoSwitch(on_ac) => {
                let enabled = match self.auto {
                    Some((_, _, ac, bat)) => !(if on_ac { ac } else { bat }),
                    None => true,
                };
                self.run_ctl("auto profile…", move |c| {
                    c.set_auto_switch(on_ac, enabled)
                })
                .await;
            }
            Act::CycleEpp => {
                if let Some(prof) = self.current_profile() {
                    self.run_ctl("EPP…", move |c| c.cycle_epp(&prof)).await;
                }
            }
            Act::Preset(name) => self.apply_preset(&name).await,
        }
    }

    fn cycle_theme(&mut self) {
        let name = crate::theme::cycle();
        self.cfg.theme = name.to_string();
        self.set_toast(format!("theme → {name}"), neon());
    }

    fn zone(&self, rect: Rect, act: Act) {
        self.zones.borrow_mut().push((rect, act));
    }

    // -- process tab helpers ----------------------------------------------

    /// Filtered + sorted process rows for the current view (top N).
    fn proc_rows(&self) -> Vec<ProcInfo> {
        let Some(s) = &self.latest else {
            return Vec::new();
        };
        let f = self.proc_filter.to_lowercase();
        let mut rows: Vec<ProcInfo> = s
            .procs_all
            .iter()
            .filter(|p| {
                f.is_empty() || p.name.to_lowercase().contains(&f) || p.pid.to_string() == f
            })
            .cloned()
            .collect();
        match self.proc_sort {
            "cpu" => rows.sort_by(|a, b| {
                b.cpu
                    .partial_cmp(&a.cpu)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "mem" => rows.sort_by(|a, b| {
                b.mem_mb
                    .partial_cmp(&a.mem_mb)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "pid" => rows.sort_by_key(|p| p.pid),
            _ => rows.sort_by_cached_key(|r| r.name.to_lowercase()),
        }
        rows.truncate(PROC_ROWS);
        rows
    }

    fn proc_name(&self, pid: u32) -> Option<String> {
        self.latest
            .as_ref()?
            .procs_all
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.clone())
    }

    async fn select_pid(&mut self, pid: u32) {
        self.selected_pid = Some(pid);
        let tel = Arc::clone(&self.tel);
        if let Ok(d) =
            tokio::task::spawn_blocking(move || tel.lock().unwrap_or_else(|e| e.into_inner()).process_detail(pid)).await
        {
            self.detail = d;
        }
    }

    async fn move_selection(&mut self, delta: i32) {
        let rows = self.proc_rows();
        if rows.is_empty() {
            return;
        }
        let cur = self
            .selected_pid
            .and_then(|pid| rows.iter().position(|p| p.pid == pid));
        let ni = match cur {
            Some(i) => (i as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
            None => 0,
        };
        let pid = rows[ni].pid;
        self.select_pid(pid).await;
    }

    fn request_kill(&mut self, force: bool) {
        match self.selected_pid {
            Some(pid) => {
                let name = self.proc_name(pid).unwrap_or_else(|| pid.to_string());
                self.confirm = Some((pid, name, force));
            }
            None => self.set_toast("no process selected".into(), amber()),
        }
    }

    async fn do_kill(&mut self, pid: u32, force: bool) {
        self.confirm = None;
        let (ok, msg) = tokio::task::spawn_blocking(move || kill_process(pid, force))
            .await
            .unwrap_or((false, "kill task failed".into()));
        self.set_toast(msg, if ok { neon() } else { red() });
        if ok {
            self.selected_pid = None;
            self.detail = None;
        }
        self.collect().await;
    }

    fn push_history(&mut self, s: &Snapshot) {
        push(&mut self.cpu_hist, s.cpu.overall);
        if let Some(t) = s.cpu.temp_c {
            push(&mut self.cputemp_hist, t);
        }
        if s.gpu.present {
            if let Some(u) = s.gpu.util {
                push(&mut self.gpu_hist, u);
            }
            if let Some(t) = s.gpu.temp_c {
                push(&mut self.gputemp_hist, t);
            }
        }
        for f in &s.fans {
            push(
                self.fan_hist.entry(f.label.clone()).or_default(),
                f.rpm as f64,
            );
        }
        let mut tot_down = 0.0;
        let mut tot_up = 0.0;
        for i in &s.net {
            if !i.is_virtual && i.is_up {
                tot_down += i.down_bps;
                tot_up += i.up_bps;
            }
        }
        push(&mut self.net_down_hist, tot_down);
        push(&mut self.net_up_hist, tot_up);
    }

    /// Rising-edge thermal alerts (cool→hot only, so we don't spam).
    fn check_alerts(&mut self, s: &Snapshot) {
        let (cpu_lim, gpu_lim, batt_lim, fan_lim) = (
            self.cfg.cpu_temp_alert,
            self.cfg.gpu_temp_alert,
            self.cfg.batt_low_pct,
            self.cfg.fan_stall_temp,
        );
        if let Some(t) = s.cpu.temp_c {
            let hot = t >= cpu_lim;
            if hot && !self.alert_cpu {
                self.alert(format!("⚠ CPU hit {t:.0}°C (≥{cpu_lim:.0}°C)"));
            }
            self.alert_cpu = hot;
        }
        if s.gpu.present {
            if let Some(t) = s.gpu.temp_c {
                let hot = t >= gpu_lim;
                if hot && !self.alert_gpu {
                    self.alert(format!("⚠ GPU hit {t:.0}°C (≥{gpu_lim:.0}°C)"));
                }
                self.alert_gpu = hot;
            }
        }
        // Battery low (rising edge), only while discharging.
        let b = &s.battery;
        if b.present {
            let discharging = b
                .rate_w
                .map(|r| r < 0.0)
                .unwrap_or(b.status.eq_ignore_ascii_case("discharging"));
            let low = b.percent.map(|p| p < batt_lim).unwrap_or(false) && discharging;
            if low && !self.batt_alerted {
                self.alert(format!("⚠ battery low: {:.0}%", b.percent.unwrap_or(0.0)));
            }
            self.batt_alerted = low;
        }
        // Fan reads 0 RPM while the CPU is hot.
        let hot = s.cpu.temp_c.map(|t| t >= fan_lim).unwrap_or(false);
        let stalled = hot && !s.fans.is_empty() && s.fans.iter().any(|f| f.rpm == 0);
        if stalled && !self.fan_alerted {
            self.alert("⚠ fan reads 0 RPM while hot".into());
        }
        self.fan_alerted = stalled;
    }

    /// Apply config rules: when discharging below a threshold, apply its preset
    /// once (rising edge tracked by `applied_rule`).
    async fn eval_rules(&mut self, s: &Snapshot) {
        if self.cfg.rules.is_empty() {
            return;
        }
        let b = &s.battery;
        let discharging = b.present
            && b.rate_w
                .map(|r| r < 0.0)
                .unwrap_or(b.status.eq_ignore_ascii_case("discharging"));
        let pct = b.percent.unwrap_or(100.0);
        let hit = if discharging {
            self.cfg
                .rules
                .iter()
                .position(|r| pct < r.battery_below as f64)
        } else {
            None
        };
        if hit != self.applied_rule {
            self.applied_rule = hit;
            if let Some(i) = hit {
                let preset = self.cfg.rules[i].preset.clone();
                self.apply_preset(&preset).await;
            }
        }
    }

    /// Apply a named preset's profile / charge limit / brightness in one shot.
    async fn apply_preset(&mut self, name: &str) {
        let Some(p) = self.cfg.preset(name).cloned() else {
            self.set_toast(format!("no preset '{name}'"), amber());
            return;
        };
        self.set_toast(format!("preset → {name}"), neon());
        let ctl = Arc::clone(&self.control);
        let _ = tokio::task::spawn_blocking(move || {
            let c = ctl.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pr) = &p.profile {
                c.set_profile(pr);
            }
            if let Some(cl) = p.charge_limit {
                c.set_charge_limit(cl);
            }
            if let Some(br) = p.brightness {
                c.set_brightness(br);
            }
        })
        .await;
        self.reload_curves().await;
    }

    fn set_toast(&mut self, msg: String, color: ratatui::style::Color) {
        self.toast = Some((msg, Instant::now(), color));
    }

    /// Fire a desktop notification (org.freedesktop.Notifications), best-effort.
    fn notify_desktop(&self, summary: &str, body: &str) {
        let Some(conn) = &self.notif else { return };
        if let Ok(p) = zbus::blocking::Proxy::new(
            conn,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        ) {
            let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
            let _: zbus::Result<u32> = p.call(
                "Notify",
                &(
                    "armoury-tui",
                    0u32,
                    "utilities-terminal",
                    summary,
                    body,
                    Vec::<&str>::new(),
                    hints,
                    6000i32,
                ),
            );
        }
    }

    /// An alert: in-TUI toast + a system notification (seen even when unfocused).
    fn alert(&mut self, msg: String) {
        self.notify_desktop("armoury-tui", &msg);
        self.set_toast(msg, red());
    }

    fn write_log(&mut self, s: &Snapshot) {
        let Some(fh) = self.log.as_mut() else { return };
        let fans: HashMap<&str, i64> = s.fans.iter().map(|f| (f.label.as_str(), f.rpm)).collect();
        let g = |v: Option<f64>, p: usize| v.map(|x| format!("{x:.*}", p)).unwrap_or_default();
        let row = format!(
            "{:.0},{},{:.1},{},{},{},{},{},{},{:.1},{},{}\n",
            s.ts,
            s.profile.clone().unwrap_or_default(),
            s.cpu.overall,
            g(s.cpu.temp_c, 0),
            if s.gpu.present {
                g(s.gpu.util, 1)
            } else {
                String::new()
            },
            g(s.gpu.temp_c, 0),
            g(s.gpu.power_w, 1),
            fans.get("cpu_fan")
                .map(|v| v.to_string())
                .unwrap_or_default(),
            fans.get("gpu_fan")
                .map(|v| v.to_string())
                .unwrap_or_default(),
            s.mem.percent,
            g(s.battery.percent, 0),
            g(s.battery.rate_w, 1),
        );
        let _ = fh.write_all(row.as_bytes());
    }
}

fn push(buf: &mut VecDeque<f64>, v: f64) {
    if buf.len() == HIST {
        buf.pop_front();
    }
    buf.push_back(v);
}

fn vec_of(d: &VecDeque<f64>) -> Vec<f64> {
    d.iter().cloned().collect()
}

const LOG_COLS: &str = "ts,profile,cpu_pct,cpu_temp,gpu_pct,gpu_temp,gpu_power_w,fan_cpu_rpm,fan_gpu_rpm,mem_pct,batt_pct,batt_rate_w";

fn open_log(path: &str) -> Option<std::fs::File> {
    let fresh = std::fs::metadata(path)
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    if fresh {
        let _ = writeln!(fh, "{LOG_COLS}");
    }
    Some(fh)
}

/// Launch the TUI: set up the terminal, run the loop, restore on exit.
pub async fn run(refresh: f64, log_path: Option<String>) -> Result<()> {
    let hw = crate::scanner::scan();
    let mut app = App::new(hw, log_path);

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let result = event_loop(&mut terminal, &mut app, refresh).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    // Persist last tab + theme for next launch.
    app.cfg.startup_tab = app.tab;
    app.cfg.theme = crate::theme::current_label().to_string();
    if let Err(e) = app.cfg.save() {
        eprintln!("Failed to save config: {}", e);
    }
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    refresh: f64,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs_f64(refresh));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Paint one frame immediately, then collect so the UI isn't blank on launch.
    terminal.draw(|f| draw(f, app))?;
    app.collect().await;
    // Default the curve editor to the live profile, then load its curves.
    if let Some(p) = app.current_profile() {
        if app.profiles.contains(&p) {
            app.curve_profile = p;
        }
    }
    app.reload_curves().await;

    loop {
        terminal.draw(|f| draw(f, app))?;
        tokio::select! {
            _ = tick.tick() => app.collect().await,
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        if app.handle_key(key).await {
                            break;
                        }
                    }
                    Some(Ok(Event::Mouse(ev))) => app.handle_mouse(ev).await,
                    Some(Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

// -- drawing ----------------------------------------------------------------

fn draw(frame: &mut Frame, app: &App) {
    app.zones.borrow_mut().clear(); // rebuild click regions each frame

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // Clickable tab bar.
    let tabs: Vec<(String, bool, Act)> = TABS
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("{} {}", i + 1, t), app.tab == i, Act::Tab(i)))
        .collect();
    place_buttons(frame, app, root[0], tabs);

    // Content.
    match app.latest.as_ref() {
        None => {
            let p = Paragraph::new("collecting telemetry…")
                .style(Style::new().fg(dim()))
                .block(Block::bordered());
            frame.render_widget(p, root[1]);
        }
        Some(s) => match app.tab {
            0 => draw_dashboard(frame, app, s, root[1]),
            1 => draw_power(frame, app, s, root[1]),
            2 => draw_lighting(frame, app, s, root[1]),
            3 => draw_network(frame, app, s, root[1]),
            4 => draw_processes(frame, app, s, root[1]),
            _ => draw_placeholder(frame, root[1]),
        },
    }

    // Footer: a transient toast, else key hints.
    let footer = match &app.toast {
        Some((msg, born, color)) if born.elapsed() < Duration::from_secs(6) => Line::styled(
            format!("  {msg}"),
            Style::new().fg(*color).add_modifier(Modifier::BOLD),
        ),
        _ => Line::from(vec![
            Span::styled("  click ", Style::new().fg(neon())),
            Span::styled("or ", Style::new().fg(dim())),
            Span::styled("1-5 ", Style::new().fg(neon())),
            Span::styled("tabs   ", Style::new().fg(dim())),
            Span::styled("r ", Style::new().fg(neon())),
            Span::styled("refresh   ", Style::new().fg(dim())),
            Span::styled("? ", Style::new().fg(neon())),
            Span::styled("help   ", Style::new().fg(dim())),
            Span::styled("q ", Style::new().fg(neon())),
            Span::styled("quit", Style::new().fg(dim())),
        ]),
    };
    frame.render_widget(Paragraph::new(footer), root[2]);

    // Kill-confirm modal on top of everything.
    if let Some((pid, name, force)) = &app.confirm {
        draw_confirm(frame, app, *pid, name, *force);
    }
    if app.help {
        draw_help(frame);
    }
}

fn draw_help(frame: &mut Frame) {
    let area = frame.area();
    let w = 64u16.min(area.width);
    let h = 22u16.min(area.height);
    let rect = Rect {
        x: (area.width - w) / 2,
        y: (area.height - h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(Span::styled(
            " KEYS & MOUSE ",
            Style::new().fg(neon()).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::new().fg(neon()));
    let row = |k: &'static str, d: &'static str| {
        Line::from(vec![
            Span::styled(format!("  {k:<10}"), Style::new().fg(neon())),
            Span::styled(d, Style::new().fg(text())),
        ])
    };
    let head = |t: &'static str| {
        Line::styled(
            format!("  {t}"),
            Style::new().fg(magenta()).add_modifier(Modifier::BOLD),
        )
    };
    let body = vec![
        head("global"),
        row("1–5 / click", "switch tab"),
        row("r", "force refresh"),
        row("t", "cycle colour theme"),
        row("? ", "this help    ·    q / Esc  quit"),
        Line::from(""),
        head("power / fans"),
        row("p", "cycle performance profile"),
        row("[ ]", "charge limit −/+ 5%      f  full-charge once"),
        row("a / b", "toggle auto profile on AC / battery"),
        row("g", "cycle EPP for active profile"),
        row("s", "switch fan-curve profile"),
        row("c / v", "fan curve cooler / quieter (±5%)"),
        row("e / d / x", "curve enable / disable / firmware default"),
        Line::from(""),
        head("lighting"),
        row("+ / -", "keyboard brightness     m  cycle aura effect"),
        Line::from(""),
        head("processes"),
        row("c m p n", "sort by CPU / MEM / PID / NAME"),
        row("/  ↑↓", "filter   ·   select     k / K  kill / force-kill"),
    ];
    frame.render_widget(Paragraph::new(Text::from(body)).block(block), rect);
}

// -- clickable widgets ------------------------------------------------------

/// Render a labelled chip and register its rect as a click zone.
fn button(frame: &mut Frame, app: &App, rect: Rect, label: &str, active: bool, act: Act) {
    let style = if active {
        Style::new()
            .fg(Color::Black)
            .bg(neon())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(text()).bg(Color::Rgb(0x1c, 0x24, 0x33))
    };
    frame.render_widget(
        Paragraph::new(format!("  {label}  "))
            .style(style)
            .alignment(Alignment::Center),
        rect,
    );
    app.zone(rect, act);
}

/// Lay chips left-to-right across one row, each sized to its label.
fn place_buttons(frame: &mut Frame, app: &App, row: Rect, btns: Vec<(String, bool, Act)>) {
    let mut x = row.x;
    let end = row.x + row.width;
    for (label, active, act) in btns {
        let w = label.chars().count() as u16 + 4;
        if x + w > end {
            break;
        }
        button(
            frame,
            app,
            Rect {
                x,
                y: row.y,
                width: w,
                height: 1,
            },
            &label,
            active,
            act,
        );
        x += w + 2;
    }
}

/// A dim label on the left of a row, then clickable chips to its right.
fn labeled_buttons(
    frame: &mut Frame,
    app: &App,
    row: Rect,
    label: &str,
    btns: Vec<(String, bool, Act)>,
) {
    let lw = 16u16.min(row.width);
    frame.render_widget(
        Paragraph::new(Span::styled(label.to_string(), Style::new().fg(dim()))),
        Rect {
            x: row.x,
            y: row.y,
            width: lw,
            height: 1,
        },
    );
    let brow = Rect {
        x: row.x + lw,
        y: row.y,
        width: row.width.saturating_sub(lw),
        height: 1,
    };
    place_buttons(frame, app, brow, btns);
}

fn draw_confirm(frame: &mut Frame, app: &App, pid: u32, name: &str, force: bool) {
    let area = frame.area();
    let w = 56u16.min(area.width);
    let h = 8u16.min(area.height);
    let rect = Rect {
        x: (area.width - w) / 2,
        y: (area.height - h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(" ⚠  KILL PROCESS ")
        .border_style(Style::new().fg(red()).add_modifier(Modifier::BOLD));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let sig = if force {
        "SIGKILL — force, no cleanup"
    } else {
        "SIGTERM — graceful"
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("{name}  (PID {pid})"),
            Style::new().fg(cyan()),
        )),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("signal: {sig}"),
            Style::new().fg(dim()),
        )),
        rows[1],
    );
    place_buttons(
        frame,
        app,
        rows[3],
        vec![
            ("Cancel (Esc)".into(), false, Act::ConfirmKill(false)),
            ("Kill (Enter)".into(), true, Act::ConfirmKill(true)),
        ],
    );
}

fn fan_hist_map(app: &App) -> HashMap<String, Vec<f64>> {
    app.fan_hist
        .iter()
        .map(|(k, v)| (k.clone(), vec_of(v)))
        .collect()
}

fn draw_dashboard(frame: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let fans = fan_hist_map(app);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),  // headline
            Constraint::Length(7),  // system + profile
            Constraint::Length(11), // cpu + gpu
            Constraint::Length(14), // thermal
            Constraint::Min(6),     // mem + fan + batt + store
        ])
        .split(area);

    frame.render_widget(render::headline_strip(s), rows[0]);

    let r1 = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[1]);
    frame.render_widget(render::system_panel(&app.hw, s), r1[0]);
    frame.render_widget(render::profile_banner(s), r1[1]);

    let r2 = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[2]);
    frame.render_widget(render::cpu_panel(s, &vec_of(&app.cpu_hist)), r2[0]);
    frame.render_widget(render::gpu_panel(s, &vec_of(&app.gpu_hist)), r2[1]);

    frame.render_widget(
        render::thermal_graph_panel(s, &vec_of(&app.cputemp_hist), &vec_of(&app.gputemp_hist)),
        rows[3],
    );

    let r4 = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 4); 4])
        .split(rows[4]);
    frame.render_widget(render::mem_panel(s), r4[0]);
    frame.render_widget(render::fan_panel(s, &fans), r4[1]);
    frame.render_widget(render::battery_panel(s), r4[2]);
    frame.render_widget(render::storage_panel(s), r4[3]);
}

fn draw_power(frame: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let fans = fan_hist_map(app);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),  // controls (profile / charge / curve / auto-epp)
            Constraint::Length(10), // fan-curve editor
            Constraint::Min(8),     // fan graph
            Constraint::Length(6),  // battery
        ])
        .split(area);

    draw_power_controls(frame, app, s, rows[0]);
    let active = app.current_profile().as_deref() == Some(app.curve_profile.as_str());
    frame.render_widget(
        render::fan_curve_panel(&app.curves, &app.curve_profile, active),
        rows[1],
    );
    frame.render_widget(render::fan_graph_panel(s, &fans), rows[2]);
    frame.render_widget(render::battery_panel(s), rows[3]);
}

/// Clickable profile / charge / fan-curve controls inside a CONTROLS panel.
fn draw_power_controls(frame: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let block = Block::bordered()
        .title(Span::styled(
            " CONTROLS ",
            Style::new().fg(neon()).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::new().fg(neon()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 {
        return;
    }
    let r = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1); 7])
        .split(inner);

    let cur = s.profile.clone();
    let profs: Vec<(String, bool, Act)> = app
        .profiles
        .iter()
        .map(|n| (n.clone(), Some(n) == cur.as_ref(), Act::Profile(n.clone())))
        .collect();
    labeled_buttons(frame, app, r[0], "profile", profs);

    let limit = s
        .battery
        .charge_limit
        .map(|l| format!("{l}%"))
        .unwrap_or_else(|| "—".into());
    labeled_buttons(
        frame,
        app,
        r[1],
        &format!("charge {limit}"),
        vec![
            ("−5%".into(), false, Act::Charge(-5)),
            ("+5%".into(), false, Act::Charge(5)),
            ("⚡ full once".into(), false, Act::FullCharge),
        ],
    );

    labeled_buttons(
        frame,
        app,
        r[2],
        &format!("curve {}", app.curve_profile),
        vec![
            ("switch".into(), false, Act::SwitchCurveProfile),
            ("cooler".into(), false, Act::CurveAdjust(5)),
            ("quieter".into(), false, Act::CurveAdjust(-5)),
            ("enable".into(), false, Act::CurveEnable(true)),
            ("disable".into(), false, Act::CurveEnable(false)),
            ("default".into(), false, Act::CurveReset),
        ],
    );

    // Auto profile (on AC / on battery) + EPP for the active profile.
    let mut auto_btns: Vec<(String, bool, Act)> = Vec::new();
    if let Some((ppoa, ppob, cac, cbat)) = app.auto {
        let pn = |i| crate::control::profile_name(i).unwrap_or("?");
        auto_btns.push((
            format!("AC:{} {}", pn(ppoa), if cac { "✓" } else { "✗" }),
            cac,
            Act::AutoSwitch(true),
        ));
        auto_btns.push((
            format!("bat:{} {}", pn(ppob), if cbat { "✓" } else { "✗" }),
            cbat,
            Act::AutoSwitch(false),
        ));
    }
    if let Some((bal, perf, quiet)) = app.epps {
        let epp = match cur.as_deref() {
            Some("Balanced") => bal,
            Some("Performance") => perf,
            Some("Quiet") => quiet,
            _ => bal,
        };
        auto_btns.push((
            format!("EPP:{}", crate::control::epp_name(epp)),
            false,
            Act::CycleEpp,
        ));
    }
    if !auto_btns.is_empty() {
        labeled_buttons(frame, app, r[3], "auto/epp", auto_btns);
    }

    let presets: Vec<(String, bool, Act)> = app
        .cfg
        .presets
        .iter()
        .map(|p| (format!("◈ {}", p.name), false, Act::Preset(p.name.clone())))
        .collect();
    if !presets.is_empty() {
        labeled_buttons(frame, app, r[4], "presets", presets);
    }

    let hint = if app.control_available {
        Span::styled(
            "p [ ] s c v e d x · f full-charge · a/b auto · g epp · ◈ presets (edit config.toml)",
            Style::new().fg(dim()),
        )
    } else {
        Span::styled(
            "asusd unreachable — controls disabled",
            Style::new().fg(red()),
        )
    };
    frame.render_widget(Paragraph::new(hint), r[5]);
}

fn draw_lighting(frame: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    frame.render_widget(lighting_panel(app, s), rows[0]);
    place_buttons(
        frame,
        app,
        rows[1],
        vec![
            ("brightness −".into(), false, Act::Brightness(-1)),
            ("brightness +".into(), false, Act::Brightness(1)),
            ("cycle aura".into(), false, Act::AuraCycle),
            ("🎨 theme".into(), false, Act::CycleTheme),
        ],
    );
    draw_swatches(frame, app, rows[2]);
    place_buttons(
        frame,
        app,
        rows[3],
        vec![
            (format!("speed: {}", app.aura_speed), false, Act::AuraSpeed),
            (format!("direction: {}", app.aura_dir), false, Act::AuraDir),
        ],
    );
}

/// Neon colour palette for the Aura editor (applies to static/breathe/highlight).
const SWATCHES: [(u8, u8, u8); 12] = [
    (0xff, 0x2e, 0x88),
    (0x36, 0xf9, 0xf6),
    (0x00, 0xff, 0x9c),
    (0xf3, 0xd0, 0x00),
    (0xff, 0x33, 0x55),
    (0x5a, 0xc8, 0xfa),
    (0x9d, 0x4e, 0xdd),
    (0xff, 0xff, 0xff),
    (0xff, 0x95, 0x00),
    (0x00, 0x47, 0xff),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
];

fn draw_swatches(frame: &mut Frame, app: &App, row: Rect) {
    frame.render_widget(
        Paragraph::new(Span::styled("colour ", Style::new().fg(dim()))),
        Rect {
            x: row.x,
            y: row.y,
            width: 8.min(row.width),
            height: 1,
        },
    );
    let mut x = row.x + 8;
    let end = row.x + row.width;
    for rgb in SWATCHES {
        let w = 3u16;
        if x + w > end {
            break;
        }
        let r = Rect {
            x,
            y: row.y,
            width: w,
            height: 1,
        };
        let active = app.aura_c1 == rgb;
        let bg = Color::Rgb(rgb.0, rgb.1, rgb.2);
        frame.render_widget(
            Paragraph::new(Span::styled(
                if active { " ◉ " } else { "   " },
                Style::new().fg(Color::Black).bg(bg),
            ))
            .alignment(Alignment::Center)
            .style(Style::new().bg(bg)),
            r,
        );
        app.zone(r, Act::AuraColor(rgb));
        x += w + 1;
    }
}

fn draw_processes(frame: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(11),
        ])
        .split(area);
    draw_proc_controls(frame, app, rows[0]);
    draw_proc_table(frame, app, rows[1]);
    let b = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[2]);
    frame.render_widget(render::proc_detail_panel(app.detail.as_ref()), b[0]);
    frame.render_widget(render::gpu_proc_panel(s), b[1]);
}

fn draw_proc_controls(frame: &mut Frame, app: &App, area: Rect) {
    let r = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    let mut btns: Vec<(String, bool, Act)> = PROC_SORTS
        .iter()
        .map(|s| (s.to_uppercase(), app.proc_sort == *s, Act::ProcSort(s)))
        .collect();
    btns.push(("⏻ kill".into(), false, Act::Kill(false)));
    btns.push(("✖ force-kill".into(), false, Act::Kill(true)));
    place_buttons(frame, app, r[0], btns);

    let cursor = if app.filtering { "_" } else { "" };
    let fcol = if app.filtering { cyan() } else { dim() };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("filter: ", Style::new().fg(dim())),
            Span::styled(
                format!("{}{}", app.proc_filter, cursor),
                Style::new().fg(fcol),
            ),
            Span::styled(
                "   ( / edit · ↑↓ select · k SIGTERM · K SIGKILL )",
                Style::new().fg(dim()),
            ),
        ])),
        r[1],
    );
}

fn draw_proc_table(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .title(Span::styled(
            " PROCESSES ",
            Style::new().fg(neon()).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::new().fg(neon()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    // Header.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{:<7}", "PID"), Style::new().fg(dim())),
            Span::styled(format!("{:<26}", "PROCESS"), Style::new().fg(dim())),
            Span::styled(format!("{:>7}", "CPU%"), Style::new().fg(dim())),
            Span::styled(format!("{:>9}", "MEM MB"), Style::new().fg(dim())),
            Span::styled(format!("{:>7}", "MEM%"), Style::new().fg(dim())),
        ])),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );
    // Rows.
    let rows = app.proc_rows();
    let visible = (inner.height - 1) as usize;
    for (i, p) in rows.iter().take(visible).enumerate() {
        let y = inner.y + 1 + i as u16;
        let rrect = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };
        let selected = Some(p.pid) == app.selected_pid;
        let name: String = p.name.chars().take(25).collect();
        let line = Line::from(vec![
            Span::styled(format!("{:<7}", p.pid), Style::new().fg(dim())),
            Span::styled(
                format!("{:<26}", name),
                Style::new().fg(if selected { neon() } else { text() }),
            ),
            Span::styled(
                format!("{:>6.1} ", p.cpu),
                Style::new().fg(render::grade((p.cpu.min(100.0)) / 100.0, true)),
            ),
            Span::styled(format!("{:>8.0} ", p.mem_mb), Style::new().fg(cyan())),
            Span::styled(
                format!("{:>6.1} ", p.mem_pct),
                Style::new().fg(render::grade(p.mem_pct / 100.0, true)),
            ),
        ]);
        let base = if selected {
            Style::new().bg(Color::Rgb(0x10, 0x2a, 0x22))
        } else {
            Style::default()
        };
        frame.render_widget(Paragraph::new(line).style(base), rrect);
        app.zone(rrect, Act::ProcSelect(p.pid));
    }
}

fn lighting_panel(app: &App, s: &Snapshot) -> Paragraph<'static> {
    let word = match s.kbd_brightness {
        Some(0) => "off",
        Some(1) => "low",
        Some(2) => "med",
        Some(3) => "high",
        _ => "n/a",
    };
    let b = s
        .kbd_brightness
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".into());
    let mode = app
        .aura_mode
        .map(crate::control::aura_mode_name)
        .unwrap_or("n/a");
    let supported: String = app
        .aura_supported
        .iter()
        .map(|m| crate::control::aura_mode_name(*m))
        .collect::<Vec<_>>()
        .join(", ");

    let row = |k: &str, v: String, vc: ratatui::style::Color| {
        Line::from(vec![
            Span::styled(format!("{:<20}", k), Style::new().fg(dim())),
            Span::styled(v, Style::new().fg(vc)),
        ])
    };
    let mut lines = vec![
        row("keyboard backlight", format!("{word} ({b})"), cyan()),
        row("aura mode", mode.to_string(), magenta()),
        row(
            "supported modes",
            if supported.is_empty() {
                "n/a".into()
            } else {
                supported
            },
            text(),
        ),
        row(
            "backend",
            if app.control_available {
                "asusd (xyz.ljones.Asusd)".into()
            } else {
                "unreachable".into()
            },
            if app.control_available { neon() } else { red() },
        ),
        row("theme", crate::theme::current().label.to_string(), neon()),
        Line::from(""),
    ];
    let key = |k: &'static str, d: &'static str| {
        Line::from(vec![
            Span::styled(k, Style::new().fg(neon())),
            Span::styled(d, Style::new().fg(dim())),
        ])
    };
    lines.push(key("+ / -   ", "brightness up / down"));
    lines.push(key("m       ", "cycle aura effect"));
    lines.push(key("t       ", "cycle colour theme"));
    render::panel(Text::from(lines), "LIGHTING", magenta())
}

fn draw_network(frame: &mut Frame, app: &App, s: &Snapshot, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(15),
            Constraint::Length(8),
            Constraint::Min(6),
        ])
        .split(area);
    frame.render_widget(
        render::bandwidth_graph_panel(
            &vec_of(&app.net_down_hist),
            &vec_of(&app.net_up_hist),
            app.net_down_hist.back().cloned().unwrap_or(0.0),
            app.net_up_hist.back().cloned().unwrap_or(0.0),
        ),
        rows[0],
    );
    frame.render_widget(render::net_table(s), rows[1]);
    frame.render_widget(render::connections_panel(&app.connections), rows[2]);
}

fn draw_placeholder(frame: &mut Frame, area: Rect) {
    let body = vec![
        Line::from(""),
        Line::styled(
            "  Processes tab arrives in Phase C.",
            Style::new().fg(amber()),
        ),
        Line::styled(
            "  Interactive table: sort / filter / select / kill, plus GPU compute apps.",
            Style::new().fg(dim()),
        ),
    ];
    frame.render_widget(
        Paragraph::new(body).block(Block::bordered().border_style(Style::new().fg(cyan()))),
        area,
    );
}
