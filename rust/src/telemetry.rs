//! Live telemetry collectors → immutable `Snapshot`.
//!
//! Port of `telemetry.py`, but reading native sources instead of scraping CLIs:
//! sysinfo (CPU / memory / processes / per-iface IP), sysfs (fans, temps,
//! battery, network counters), NVML via nvml-wrapper (GPU), and UPower over
//! D-Bus (blocking zbus) for accurate battery wattage. Every field is optional:
//! absent hardware yields `None` so the renderer shows "n/a" instead of dying.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::Nvml;
use sysinfo::{Networks, ProcessesToUpdate, System};

use crate::scanner::{HardwareMap, ACPI_PROFILE, KBD_LED};
use crate::sysfs;

// Interface name prefixes we consider "virtual" (shown but de-emphasised).
const VIRTUAL_IFACE: &[&str] = &[
    "lo", "docker", "veth", "br-", "virbr", "vmnet", "mpqemu", "tun", "tap",
];

// ---------------------------------------------------------------------------
// Per-subsystem snapshot records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct CpuSample {
    pub per_core: Vec<f64>,
    pub freqs_mhz: Vec<f64>,
    pub overall: f64,
    pub temp_c: Option<f64>,
    pub load1: Option<f64>,
    pub cores: usize,
}

#[derive(Debug, Clone, Default)]
pub struct GpuSample {
    pub present: bool,
    pub vendor: String,
    pub name: String,
    pub util: Option<f64>,
    pub mem_used_mb: Option<f64>,
    pub mem_total_mb: Option<f64>,
    pub temp_c: Option<f64>,
    pub power_w: Option<f64>,
    pub clock_mhz: Option<f64>,
    pub fan_pct: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct FanSample {
    pub label: String,
    pub rpm: i64,
}

#[derive(Debug, Clone, Default)]
pub struct MemSample {
    pub used: u64,
    pub total: u64,
    pub percent: f64,
    pub swap_used: u64,
    pub swap_total: u64,
}

#[derive(Debug, Clone, Default)]
pub struct StorageSample {
    pub nvme_temp_c: Option<f64>,
    pub root_used: u64,
    pub root_total: u64,
    pub root_percent: f64,
}

#[derive(Debug, Clone, Default)]
pub struct BatterySample {
    pub present: bool,
    pub percent: Option<f64>,
    pub status: String,
    pub rate_w: Option<f64>, // signed: + charging, - discharging
    pub charge_limit: Option<i64>,
    pub ac_online: Option<bool>,
    pub health_pct: Option<f64>,    // full / design capacity
    pub cycle_count: Option<i64>,
    pub time_to_empty_s: Option<i64>,
    pub time_to_full_s: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct NetIface {
    pub name: String,
    pub up_bps: f64,
    pub down_bps: f64,
    pub is_up: bool,
    pub is_virtual: bool,
    pub ipv4: String,
    // Collected for the Phase C interface-detail view; not in the Phase A table.
    #[allow(dead_code)]
    pub mac: String,
    #[allow(dead_code)]
    pub mtu: i64,
    #[allow(dead_code)]
    pub speed: i64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub errin: u64,
    pub errout: u64,
    pub dropin: u64,
    pub dropout: u64,
}

/// One active inet socket (Network tab, on-demand).
#[derive(Debug, Clone)]
pub struct NetConn {
    pub proto: &'static str,
    pub laddr: String,
    pub raddr: String,
    pub status: String,
    pub pname: String,
    pub pid: Option<i32>,
}

// pid / mem_pct / name feed the Phase C interactive process & GPU-process tables.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f64,
    pub mem_mb: f64,
    pub mem_pct: f64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GpuProc {
    pub pid: u32,
    pub name: String,
    pub mem_mb: f64,
}

/// Detail card for the selected process (Phase C).
#[derive(Debug, Clone)]
pub struct ProcDetail {
    pub pid: u32,
    pub name: String,
    pub status: String,
    pub user: String,
    pub ppid: Option<u32>,
    pub cpu: f64,
    pub mem_mb: f64,
    pub start_time: u64,
    pub cmd: String,
}

/// Send SIGTERM (or SIGKILL if `force`) to a process, with guard rails.
/// Refuses PID 1 and this process / its parent. Never panics.
pub fn kill_process(pid: u32, force: bool) -> (bool, String) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    let own = std::process::id();
    let parent = nix::unistd::getppid().as_raw() as u32;
    if pid == 1 || pid == own || pid == parent {
        return (false, format!("refusing to kill critical/own process {pid}"));
    }
    let sig = if force { Signal::SIGKILL } else { Signal::SIGTERM };
    match kill(Pid::from_raw(pid as i32), sig) {
        Ok(()) => (true, format!("sent {sig:?} → {pid}")),
        Err(nix::errno::Errno::ESRCH) => (false, format!("process {pid} is already gone")),
        Err(nix::errno::Errno::EPERM) => (false, format!("permission denied for {pid} — likely root-owned")),
        Err(e) => (false, format!("failed to signal {pid}: {e}")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub ts: f64,
    pub uptime_s: f64,
    pub profile: Option<String>,
    pub kbd_brightness: Option<i64>,
    pub cpu: CpuSample,
    pub gpu: GpuSample,
    pub fans: Vec<FanSample>,
    pub mem: MemSample,
    pub storage: StorageSample,
    pub battery: BatterySample,
    pub net: Vec<NetIface>,
    pub procs_cpu: Vec<ProcInfo>,
    // Populated now; consumed by the Phase C process tab (sort/filter/kill).
    #[allow(dead_code)]
    pub procs_mem: Vec<ProcInfo>,
    #[allow(dead_code)]
    pub procs_all: Vec<ProcInfo>,
    #[allow(dead_code)]
    pub gpu_procs: Vec<GpuProc>,
}

pub struct Telemetry {
    hw: HardwareMap,
    sys: System,
    networks: Networks,
    nvml: Option<Nvml>,
    prev_net: HashMap<String, (u64, u64)>, // name -> (tx_total, rx_total)
    prev_net_ts: Option<Instant>,
    upower_conn: Option<zbus::blocking::Connection>,
    upower_path: Option<String>,
}

impl Telemetry {
    pub fn new(hw: HardwareMap) -> Self {
        let mut sys = System::new();
        // Prime CPU% so the first real reading is a delta, not a since-boot value
        // (mirrors the psutil priming in telemetry.py).
        sys.refresh_cpu_all();

        let nvml = if hw.gpu_vendor == "nvidia" {
            Nvml::init().ok()
        } else {
            None
        };

        let mut t = Telemetry {
            hw,
            sys,
            networks: Networks::new_with_refreshed_list(),
            nvml,
            prev_net: HashMap::new(),
            prev_net_ts: None,
            upower_conn: None,
            upower_path: None,
        };
        t.resolve_upower();
        t
    }

    // -- UPower (D-Bus) ---------------------------------------------------

    /// Open a system-bus connection and find the battery device path once.
    fn resolve_upower(&mut self) {
        let Ok(conn) = zbus::blocking::Connection::system() else { return };
        let devices: Vec<zbus::zvariant::OwnedObjectPath> = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.UPower",
            "/org/freedesktop/UPower",
            "org.freedesktop.UPower",
        )
        .ok()
        .and_then(|p| p.call("EnumerateDevices", &()).ok())
        .unwrap_or_default();
        self.upower_path = devices
            .into_iter()
            .map(|p| p.as_str().to_string())
            .find(|p| p.contains("battery_BAT") || p.ends_with("/battery"));
        self.upower_conn = Some(conn);
    }

    /// Battery energy-rate magnitude in watts from UPower, if available.
    fn upower_energy_rate(&self) -> Option<f64> {
        let conn = self.upower_conn.as_ref()?;
        let path = self.upower_path.as_ref()?;
        let proxy = zbus::blocking::Proxy::new(
            conn,
            "org.freedesktop.UPower",
            path.as_str(),
            "org.freedesktop.UPower.Device",
        )
        .ok()?;
        let rate: f64 = proxy.get_property("EnergyRate").ok()?;
        Some(rate.abs())
    }

    // -- CPU --------------------------------------------------------------

    fn cpu(&mut self) -> CpuSample {
        self.sys.refresh_cpu_all();
        let per_core: Vec<f64> = self.sys.cpus().iter().map(|c| c.cpu_usage() as f64).collect();
        let freqs_mhz: Vec<f64> = self.sys.cpus().iter().map(|c| c.frequency() as f64).collect();
        let overall = if per_core.is_empty() {
            0.0
        } else {
            per_core.iter().sum::<f64>() / per_core.len() as f64
        };
        let load1 = {
            let la = System::load_average();
            if la.one >= 0.0 {
                Some(la.one)
            } else {
                None
            }
        };
        let temp = self.hw.cpu_temp.as_ref().and_then(|c| sysfs::read_milli(&c.path));
        CpuSample {
            cores: per_core.len(),
            per_core,
            freqs_mhz,
            overall,
            temp_c: temp,
            load1,
        }
    }

    // -- GPU --------------------------------------------------------------

    fn gpu(&self) -> GpuSample {
        if let Some(nvml) = &self.nvml {
            return self.gpu_nvidia(nvml);
        }
        if self.hw.gpu_vendor == "amd" {
            if let Some(dir) = &self.hw.amd_gpu_hwmon {
                return gpu_amd(dir);
            }
        }
        GpuSample {
            present: false,
            vendor: self.hw.gpu_vendor.clone(),
            ..Default::default()
        }
    }

    fn gpu_nvidia(&self, nvml: &Nvml) -> GpuSample {
        let Ok(dev) = nvml.device_by_index(0) else {
            return GpuSample { present: false, vendor: "nvidia".into(), ..Default::default() };
        };
        let mem = dev.memory_info().ok();
        GpuSample {
            present: true,
            vendor: "nvidia".into(),
            name: dev.name().unwrap_or_else(|_| "NVIDIA GPU".into()),
            util: dev.utilization_rates().ok().map(|u| u.gpu as f64),
            mem_used_mb: mem.as_ref().map(|m| m.used as f64 / 1_048_576.0),
            mem_total_mb: mem.as_ref().map(|m| m.total as f64 / 1_048_576.0),
            temp_c: dev.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f64),
            power_w: dev.power_usage().ok().map(|mw| mw as f64 / 1000.0),
            clock_mhz: dev.clock_info(Clock::Graphics).ok().map(|c| c as f64),
            fan_pct: dev.fan_speed(0).ok().map(|f| f as f64),
        }
    }

    // -- fans -------------------------------------------------------------

    fn fans(&self) -> Vec<FanSample> {
        self.hw
            .fans
            .iter()
            .filter_map(|f| sysfs::read_int(&f.path).map(|rpm| FanSample { label: f.label.clone(), rpm }))
            .collect()
    }

    // -- memory -----------------------------------------------------------

    fn mem(&mut self) -> MemSample {
        self.sys.refresh_memory();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        let percent = if total > 0 { used as f64 / total as f64 * 100.0 } else { 0.0 };
        MemSample {
            used,
            total,
            percent,
            swap_used: self.sys.used_swap(),
            swap_total: self.sys.total_swap(),
        }
    }

    // -- storage ----------------------------------------------------------

    fn storage(&self) -> StorageSample {
        let temp = self.hw.nvme_temp.as_ref().and_then(|c| sysfs::read_milli(&c.path));
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let mut root_used = 0;
        let mut root_total = 0;
        for d in disks.list() {
            if d.mount_point() == Path::new("/") {
                root_total = d.total_space();
                root_used = root_total.saturating_sub(d.available_space());
                break;
            }
        }
        let pct = if root_total > 0 { root_used as f64 / root_total as f64 * 100.0 } else { 0.0 };
        StorageSample {
            nvme_temp_c: temp,
            root_used,
            root_total,
            root_percent: pct,
        }
    }

    // -- battery ----------------------------------------------------------

    fn battery(&self) -> BatterySample {
        let Some(bat) = &self.hw.battery else {
            return BatterySample::default();
        };
        let percent = sysfs::read_float(bat.join("capacity"));
        let status = sysfs::read_text(bat.join("status")).unwrap_or_else(|| "unknown".into());
        let charge_limit = self.hw.charge_limit_node.as_ref().and_then(sysfs::read_int);
        let ac_online = self
            .hw
            .ac_adapter
            .as_ref()
            .and_then(|a| sysfs::read_int(a.join("online")))
            .map(|v| v != 0);
        let rate = self.battery_rate(bat, &status);
        // Health = full / design capacity (charge_* in µAh, else energy_* in µWh).
        let health_pct = capacity_ratio(bat, "charge").or_else(|| capacity_ratio(bat, "energy"));
        let cycle_count = sysfs::read_int(bat.join("cycle_count"));
        let (time_to_empty_s, time_to_full_s) = self.upower_times();
        BatterySample {
            present: true,
            percent,
            status,
            rate_w: rate,
            charge_limit,
            ac_online,
            health_pct,
            cycle_count,
            time_to_empty_s,
            time_to_full_s,
        }
    }

    /// UPower TimeToEmpty / TimeToFull in seconds (0 → unknown → None).
    fn upower_times(&self) -> (Option<i64>, Option<i64>) {
        let (Some(conn), Some(path)) = (self.upower_conn.as_ref(), self.upower_path.as_ref()) else {
            return (None, None);
        };
        let Ok(proxy) = zbus::blocking::Proxy::new(conn, "org.freedesktop.UPower", path.as_str(), "org.freedesktop.UPower.Device") else {
            return (None, None);
        };
        let pos = |v: zbus::Result<i64>| v.ok().filter(|&s| s > 0);
        (pos(proxy.get_property("TimeToEmpty")), pos(proxy.get_property("TimeToFull")))
    }

    /// Signed power flow in watts (+ charging, - discharging). Prefers UPower's
    /// accurate energy-rate, then falls back to sysfs power_now / I·V.
    fn battery_rate(&self, bat: &Path, status: &str) -> Option<f64> {
        let mag = self.upower_energy_rate().or_else(|| {
            if let Some(p) = sysfs::read_int(bat.join("power_now")) {
                Some(p.unsigned_abs() as f64 / 1_000_000.0)
            } else {
                let i = sysfs::read_int(bat.join("current_now"))?;
                let v = sysfs::read_int(bat.join("voltage_now"))?;
                Some(i.unsigned_abs() as f64 * v.unsigned_abs() as f64 / 1e12)
            }
        })?;
        Some(if status.to_lowercase().starts_with("dis") { -mag } else { mag })
    }

    // -- network ----------------------------------------------------------

    fn net(&mut self) -> Vec<NetIface> {
        let now = Instant::now();
        self.networks.refresh(true);
        // name -> first IPv4 string, from sysinfo (sysfs can't give this).
        let mut ipv4_map: HashMap<String, String> = HashMap::new();
        for (name, data) in &self.networks {
            if let Some(ip) = data.ip_networks().iter().find(|n| n.addr.is_ipv4()) {
                ipv4_map.insert(name.clone(), ip.addr.to_string());
            }
        }

        let dt = self.prev_net_ts.map(|t| now.duration_since(t).as_secs_f64());
        let mut result: Vec<NetIface> = Vec::new();
        for dir in sysfs::list_dir("/sys/class/net") {
            let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()) else {
                continue;
            };
            let stats = dir.join("statistics");
            let rx = sysfs::read_int(stats.join("rx_bytes")).unwrap_or(0) as u64;
            let tx = sysfs::read_int(stats.join("tx_bytes")).unwrap_or(0) as u64;
            let (up, down) = match (self.prev_net.get(&name), dt) {
                (Some(&(ptx, prx)), Some(dt)) if dt > 0.0 => {
                    (tx.saturating_sub(ptx) as f64 / dt, rx.saturating_sub(prx) as f64 / dt)
                }
                _ => (0.0, 0.0),
            };
            self.prev_net.insert(name.clone(), (tx, rx));

            // Match psutil's is_up: the IFF_UP flag (0x1), not operstate — some
            // NICs (e.g. USB ethernet) report operstate "unknown" while up.
            let is_up = sysfs::read_int(dir.join("flags")).map(|f| f & 0x1 != 0).unwrap_or(false);
            let speed = sysfs::read_int(dir.join("speed")).filter(|&s| s > 0).unwrap_or(0);
            let is_virtual = VIRTUAL_IFACE.iter().any(|p| name.starts_with(p));
            result.push(NetIface {
                up_bps: up,
                down_bps: down,
                is_up,
                is_virtual,
                ipv4: ipv4_map.get(&name).cloned().unwrap_or_default(),
                mac: sysfs::read_text(dir.join("address")).unwrap_or_default(),
                mtu: sysfs::read_int(dir.join("mtu")).unwrap_or(0),
                speed,
                rx_total: rx,
                tx_total: tx,
                errin: sysfs::read_int(stats.join("rx_errors")).unwrap_or(0) as u64,
                errout: sysfs::read_int(stats.join("tx_errors")).unwrap_or(0) as u64,
                dropin: sysfs::read_int(stats.join("rx_dropped")).unwrap_or(0) as u64,
                dropout: sysfs::read_int(stats.join("tx_dropped")).unwrap_or(0) as u64,
                name,
            });
        }
        self.prev_net_ts = Some(now);
        // Real, up interfaces first; then by throughput.
        result.sort_by(|a, b| {
            (a.is_virtual, !a.is_up, -(a.up_bps + a.down_bps))
                .partial_cmp(&(b.is_virtual, !b.is_up, -(b.up_bps + b.down_bps)))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        result
    }

    // -- processes --------------------------------------------------------

    fn processes(&mut self) -> (Vec<ProcInfo>, Vec<ProcInfo>, Vec<ProcInfo>) {
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        let total = self.sys.total_memory().max(1) as f64;
        let mut rows: Vec<ProcInfo> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let mem = p.memory() as f64; // bytes (RSS)
                ProcInfo {
                    pid: pid.as_u32(),
                    name: p.name().to_string_lossy().to_string(),
                    cpu: p.cpu_usage() as f64,
                    mem_mb: mem / 1_048_576.0,
                    mem_pct: mem / total * 100.0,
                }
            })
            .collect();

        let mut by_cpu = rows.clone();
        by_cpu.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
        by_cpu.truncate(7);
        let mut by_mem = rows.clone();
        by_mem.sort_by(|a, b| b.mem_mb.partial_cmp(&a.mem_mb).unwrap_or(std::cmp::Ordering::Equal));
        by_mem.truncate(7);
        rows.shrink_to_fit();
        (by_cpu, by_mem, rows)
    }

    /// Best-effort detail bundle for one process (each field guarded).
    pub fn process_detail(&mut self, pid: u32) -> Option<ProcDetail> {
        let spid = sysinfo::Pid::from_u32(pid);
        self.sys.refresh_processes(ProcessesToUpdate::Some(&[spid]), false);
        let users = sysinfo::Users::new_with_refreshed_list();
        let p = self.sys.process(spid)?;
        let user = p
            .user_id()
            .and_then(|uid| users.get_user_by_id(uid))
            .map(|u| u.name().to_string())
            .unwrap_or_else(|| "—".into());
        let cmd = p
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        Some(ProcDetail {
            pid,
            name: p.name().to_string_lossy().to_string(),
            status: format!("{:?}", p.status()).to_lowercase(),
            user,
            ppid: p.parent().map(|pp| pp.as_u32()),
            cpu: p.cpu_usage() as f64,
            mem_mb: p.memory() as f64 / 1_048_576.0,
            start_time: p.start_time(),
            cmd: if cmd.is_empty() { "—".into() } else { cmd },
        })
    }

    /// Active inet sockets, ESTABLISHED first, with owning process names.
    /// Reads /proc as the normal user; sockets we don't own report pid=None.
    pub fn connections(&self, limit: usize) -> Vec<NetConn> {
        // Map socket inode -> (pid, comm) by scanning every process's fds.
        let mut owner: HashMap<u64, (i32, String)> = HashMap::new();
        if let Ok(procs) = procfs::process::all_processes() {
            for p in procs.flatten() {
                let comm = p.stat().map(|s| s.comm).unwrap_or_default();
                if let Ok(fds) = p.fd() {
                    for fd in fds.flatten() {
                        if let procfs::process::FDTarget::Socket(ino) = fd.target {
                            owner.entry(ino).or_insert((p.pid, comm.clone()));
                        }
                    }
                }
            }
        }
        let mut out: Vec<NetConn> = Vec::new();
        let mut push = |proto: &'static str, laddr: String, raddr: String, status: String, inode: u64| {
            let (pid, pname) = match owner.get(&inode) {
                Some((pid, name)) => (Some(*pid), name.clone()),
                None => (None, "—".into()),
            };
            out.push(NetConn { proto, laddr, raddr, status, pname, pid });
        };
        if let Ok(t) = procfs::net::tcp() {
            for e in t {
                push("tcp", e.local_address.to_string(), e.remote_address.to_string(), format!("{:?}", e.state).to_uppercase(), e.inode);
            }
        }
        if let Ok(t) = procfs::net::tcp6() {
            for e in t {
                push("tcp6", e.local_address.to_string(), e.remote_address.to_string(), format!("{:?}", e.state).to_uppercase(), e.inode);
            }
        }
        if let Ok(u) = procfs::net::udp() {
            for e in u {
                push("udp", e.local_address.to_string(), e.remote_address.to_string(), "-".into(), e.inode);
            }
        }
        let rank = |s: &str| match s {
            "ESTABLISHED" => 0,
            "LISTEN" => 1,
            _ => 2,
        };
        out.sort_by_key(|c| (rank(&c.status), c.proto));
        out.truncate(limit);
        out
    }

    fn gpu_processes(&self) -> Vec<GpuProc> {
        let Some(nvml) = &self.nvml else { return Vec::new() };
        let Ok(dev) = nvml.device_by_index(0) else { return Vec::new() };
        let Ok(procs) = dev.running_compute_processes() else { return Vec::new() };
        let mut out: Vec<GpuProc> = procs
            .into_iter()
            .map(|p| {
                let mem_mb = match p.used_gpu_memory {
                    nvml_wrapper::enums::device::UsedGpuMemory::Used(b) => b as f64 / 1_048_576.0,
                    _ => 0.0,
                };
                let name = self
                    .sys
                    .process(sysinfo::Pid::from_u32(p.pid))
                    .map(|pr| pr.name().to_string_lossy().to_string())
                    .unwrap_or_else(|| "?".into());
                GpuProc { pid: p.pid, name, mem_mb }
            })
            .collect();
        out.sort_by(|a, b| b.mem_mb.partial_cmp(&a.mem_mb).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    // -- misc -------------------------------------------------------------

    fn profile(&self) -> Option<String> {
        let raw = sysfs::read_text(ACPI_PROFILE)?;
        if raw.is_empty() {
            return None;
        }
        let mut chars = raw.chars();
        Some(chars.next().unwrap().to_uppercase().collect::<String>() + chars.as_str())
    }

    fn brightness(&self) -> Option<i64> {
        if !self.hw.has_kbd_backlight {
            return None;
        }
        sysfs::read_int(Path::new(KBD_LED).join("brightness"))
    }

    fn uptime() -> f64 {
        sysfs::read_text("/proc/uptime")
            .and_then(|raw| raw.split_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(0.0)
    }

    // -- public -----------------------------------------------------------

    /// Collect one full reading of every subsystem.
    pub fn snapshot(&mut self) -> Snapshot {
        // Collect CPU first: refresh_processes() also updates the global CPU
        // baseline, so reading cpu() after it would collapse the usage delta to
        // ~0. Measuring CPU first keeps the delta spanning the previous tick.
        let cpu = self.cpu();
        let (procs_cpu, procs_mem, procs_all) = self.processes();
        Snapshot {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            uptime_s: Self::uptime(),
            profile: self.profile(),
            kbd_brightness: self.brightness(),
            cpu,
            gpu: self.gpu(),
            fans: self.fans(),
            mem: self.mem(),
            storage: self.storage(),
            battery: self.battery(),
            net: self.net(),
            procs_cpu,
            procs_mem,
            procs_all,
            gpu_procs: self.gpu_processes(),
        }
    }
}

/// full / design capacity as a percent, for `kind` in {"charge","energy"}.
fn capacity_ratio(bat: &Path, kind: &str) -> Option<f64> {
    let full = sysfs::read_int(bat.join(format!("{kind}_full")))?;
    let design = sysfs::read_int(bat.join(format!("{kind}_full_design")))?;
    (design > 0).then(|| full as f64 / design as f64 * 100.0)
}

fn gpu_amd(dir: &Path) -> GpuSample {
    let temp = sysfs::read_milli(dir.join("temp1_input"));
    // power1_average is µW; read_milli gives mW, /1000 -> W.
    let power = sysfs::read_milli(dir.join("power1_average")).map(|mw| mw / 1000.0);
    let busy = std::fs::canonicalize(dir.join("device"))
        .ok()
        .and_then(|d| sysfs::read_int(d.join("gpu_busy_percent")));
    GpuSample {
        present: true,
        vendor: "amd".into(),
        name: "AMD GPU".into(),
        util: busy.map(|b| b as f64),
        temp_c: temp,
        power_w: power,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// `--once`: print one plain-text telemetry snapshot and exit.
// ---------------------------------------------------------------------------

pub fn once() -> Result<()> {
    use crate::render::{fmt_rate, human_bytes};

    let hw = crate::scanner::scan();
    let mut tel = Telemetry::new(hw);
    // Two reads ~0.4s apart so CPU% and network rates are meaningful.
    tel.snapshot();
    std::thread::sleep(Duration::from_millis(400));
    let s = tel.snapshot();

    println!("profile     : {}", s.profile.as_deref().unwrap_or("None"));
    println!(
        "cpu         : {:.1}%  {:.0}°C  {} cores",
        s.cpu.overall,
        s.cpu.temp_c.unwrap_or(0.0),
        s.cpu.cores
    );
    if s.gpu.present {
        println!(
            "gpu         : {}  {:.0}%  {:.0}°C  {:.1}W",
            s.gpu.name,
            s.gpu.util.unwrap_or(0.0),
            s.gpu.temp_c.unwrap_or(0.0),
            s.gpu.power_w.unwrap_or(0.0)
        );
    }
    for f in &s.fans {
        println!("fan         : {:<10} {} rpm", f.label, f.rpm);
    }
    println!(
        "memory      : {}/{} ({:.0}%)",
        human_bytes(s.mem.used as f64),
        human_bytes(s.mem.total as f64),
        s.mem.percent
    );
    if let Some(t) = s.storage.nvme_temp_c {
        println!("nvme        : {:.0}°C", t);
    }
    let b = &s.battery;
    if b.present {
        let rate = b.rate_w.map(|r| format!("{:+.1}W", r)).unwrap_or_else(|| "n/a".into());
        println!(
            "battery     : {:.0}%  {}  rate={}  limit={}%",
            b.percent.unwrap_or(0.0),
            b.status,
            rate,
            b.charge_limit.map(|l| l.to_string()).unwrap_or_else(|| "None".into())
        );
    }
    println!(
        "kbd light   : {}",
        s.kbd_brightness.map(|v| v.to_string()).unwrap_or_else(|| "None".into())
    );
    for i in &s.net {
        if i.is_up && !i.is_virtual {
            println!(
                "net         : {:<8} ↓{} ↑{}",
                i.name,
                fmt_rate(i.down_bps),
                fmt_rate(i.up_bps)
            );
        }
    }
    let top: Vec<String> = s
        .procs_cpu
        .iter()
        .take(3)
        .map(|p| format!("{}({:.0}%)", p.name, p.cpu))
        .collect();
    println!("top cpu     : {}", top.join(", "));
    Ok(())
}

/// `--json`: one snapshot as a single JSON line (status bars / scripting).
pub fn json() -> Result<()> {
    use serde::Serialize;
    #[derive(Serialize)]
    struct J {
        profile: Option<String>,
        cpu_load: f64,
        cpu_temp: Option<f64>,
        cpu_cores: usize,
        gpu_present: bool,
        gpu_util: Option<f64>,
        gpu_temp: Option<f64>,
        gpu_power_w: Option<f64>,
        fans_rpm: Vec<(String, i64)>,
        mem_pct: f64,
        nvme_temp: Option<f64>,
        battery_pct: Option<f64>,
        battery_status: String,
        battery_rate_w: Option<f64>,
        battery_health_pct: Option<f64>,
        charge_limit: Option<i64>,
        kbd_brightness: Option<i64>,
        net_down_bps: f64,
        net_up_bps: f64,
        uptime_s: f64,
    }
    let r1 = |x: f64| (x * 10.0).round() / 10.0;

    let hw = crate::scanner::scan();
    let mut tel = Telemetry::new(hw);
    tel.snapshot();
    std::thread::sleep(Duration::from_millis(400));
    let s = tel.snapshot();
    let (down, up) = s
        .net
        .iter()
        .filter(|i| i.is_up && !i.is_virtual)
        .fold((0.0, 0.0), |(d, u), i| (d + i.down_bps, u + i.up_bps));

    let j = J {
        profile: s.profile.clone(),
        cpu_load: r1(s.cpu.overall),
        cpu_temp: s.cpu.temp_c,
        cpu_cores: s.cpu.cores,
        gpu_present: s.gpu.present,
        gpu_util: s.gpu.util,
        gpu_temp: s.gpu.temp_c,
        gpu_power_w: s.gpu.power_w,
        fans_rpm: s.fans.iter().map(|f| (f.label.clone(), f.rpm)).collect(),
        mem_pct: r1(s.mem.percent),
        nvme_temp: s.storage.nvme_temp_c,
        battery_pct: s.battery.percent,
        battery_status: s.battery.status.clone(),
        battery_rate_w: s.battery.rate_w,
        battery_health_pct: s.battery.health_pct.map(r1),
        charge_limit: s.battery.charge_limit,
        kbd_brightness: s.kbd_brightness,
        net_down_bps: down,
        net_up_bps: up,
        uptime_s: s.uptime_s,
    };
    println!("{}", serde_json::to_string(&j).unwrap_or_default());
    Ok(())
}

/// `--replay`: summarise a `--log` CSV (per-metric min/avg/max + a sparkline).
pub fn replay(path: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?;
    let mut lines = text.lines();
    let cols: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();
    let idx = |name: &str| cols.iter().position(|c| *c == name);
    let want: [(&str, &str, &str); 6] = [
        ("cpu_pct", "cpu load", "%"),
        ("cpu_temp", "cpu temp", "°C"),
        ("gpu_temp", "gpu temp", "°C"),
        ("gpu_power_w", "gpu power", "W "),
        ("mem_pct", "memory", "% "),
        ("batt_pct", "battery", "% "),
    ];
    let ts_i = idx("ts");
    let mut series: Vec<Vec<f64>> = vec![Vec::new(); want.len()];
    let mut tss: Vec<f64> = Vec::new();
    let mut rows = 0usize;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        let get = |i: Option<usize>| i.and_then(|i| f.get(i)).and_then(|v| v.parse::<f64>().ok());
        if let Some(t) = get(ts_i) {
            tss.push(t);
        }
        for (k, (name, _, _)) in want.iter().enumerate() {
            if let Some(v) = get(idx(name)) {
                series[k].push(v);
            }
        }
        rows += 1;
    }
    if rows == 0 {
        println!("no data rows in {path}");
        return Ok(());
    }
    let dur = match (tss.first(), tss.last()) {
        (Some(a), Some(b)) => b - a,
        _ => 0.0,
    };
    println!("replay {path}: {rows} samples over {dur:.0}s");
    for (k, (_, label, unit)) in want.iter().enumerate() {
        let v = &series[k];
        if v.is_empty() {
            continue;
        }
        let mn = v.iter().cloned().fold(f64::INFINITY, f64::min);
        let mx = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        println!("  {label:<9} min {mn:>5.0} avg {avg:>5.0} max {mx:>5.0} {unit}  {}", spark(v));
    }
    Ok(())
}

fn spark(v: &[f64]) -> String {
    const B: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = if (hi - lo).abs() < f64::EPSILON { 1.0 } else { hi - lo };
    let step = (v.len() as f64 / 50.0).max(1.0);
    let mut out = String::new();
    let mut i = 0.0;
    while (i as usize) < v.len() {
        let idx = (((v[i as usize] - lo) / span) * 7.0).round() as usize;
        out.push(B[idx.min(7)]);
        i += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- spark() tests -----------------------------------------------------

    #[test]
    fn test_spark_empty() {
        // Empty slice: lo=+inf, hi=-inf, span=1.0, loop body never runs → empty string
        let result = spark(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_spark_single_value() {
        let result = spark(&[42.0]);
        // Single value: lo==hi, span=1.0, (42-42)/1*7 = 0 → first block char
        assert_eq!(result.chars().count(), 1); // one character for one sample
        assert_eq!(result.chars().next().unwrap(), '▁');
    }

    #[test]
    fn test_spark_constant_values() {
        // All the same value: lo==hi, span=1.0, every value maps to index 0
        let result = spark(&[5.0, 5.0, 5.0, 5.0, 5.0]);
        assert!(!result.is_empty());
        // All characters should be the same (lowest block)
        for ch in result.chars() {
            assert_eq!(ch, '▁');
        }
    }

    #[test]
    fn test_spark_ascending_values() {
        // Ascending 0..7 should produce increasingly taller blocks
        let vals: Vec<f64> = (0..8).map(|i| i as f64).collect();
        let result = spark(&vals);
        assert!(!result.is_empty());
        let chars: Vec<char> = result.chars().collect();
        // First char should be lowest block, last should be highest
        assert_eq!(chars[0], '▁');
        assert_eq!(*chars.last().unwrap(), '█');
    }

    #[test]
    fn test_spark_two_values() {
        let result = spark(&[0.0, 100.0]);
        let chars: Vec<char> = result.chars().collect();
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[0], '▁'); // min → index 0
        assert_eq!(chars[1], '█'); // max → index 7
    }

    // -- capacity_ratio() tests -------------------------------------------

    #[test]
    fn test_capacity_ratio_normal() {
        let dir = std::env::temp_dir().join(format!("telem_cr_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("charge_full"), "80").unwrap();
        std::fs::write(dir.join("charge_full_design"), "100").unwrap();
        let ratio = capacity_ratio(&dir, "charge");
        assert!(ratio.is_some());
        assert!((ratio.unwrap() - 80.0).abs() < 0.01);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_capacity_ratio_design_zero() {
        let dir = std::env::temp_dir().join(format!("telem_cr0_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("energy_full"), "80").unwrap();
        std::fs::write(dir.join("energy_full_design"), "0").unwrap();
        let ratio = capacity_ratio(&dir, "energy");
        // design=0 → (0 > 0) is false → None
        assert!(ratio.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_capacity_ratio_missing_files() {
        let dir = std::env::temp_dir().join(format!("telem_crm_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // No charge_full or charge_full_design files
        let ratio = capacity_ratio(&dir, "charge");
        assert!(ratio.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_capacity_ratio_partial_files() {
        let dir = std::env::temp_dir().join(format!("telem_crp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Only full, no design
        std::fs::write(dir.join("charge_full"), "80").unwrap();
        let ratio = capacity_ratio(&dir, "charge");
        assert!(ratio.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- kill_process() tests ---------------------------------------------

    #[test]
    fn test_kill_process_pid_1() {
        let (ok, msg) = kill_process(1, false);
        assert!(!ok);
        assert!(msg.contains("refusing"));
    }

    #[test]
    fn test_kill_process_own_pid() {
        let own = std::process::id();
        let (ok, msg) = kill_process(own, false);
        assert!(!ok);
        assert!(msg.contains("refusing"));
    }

    #[test]
    fn test_kill_process_nonexistent() {
        // Use a very high PID that almost certainly doesn't exist
        let (ok, msg) = kill_process(99_999_999, false);
        assert!(!ok);
        // Should report "already gone" (ESRCH) or "permission denied" (EPERM)
        assert!(msg.contains("already gone") || msg.contains("permission denied") || msg.contains("failed"));
    }
}
