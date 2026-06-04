//! Module 1 — the hardware scanner.
//!
//! Never hardcode `hwmonN`: the kernel numbers those nodes in probe order, so
//! they reshuffle between boots. We enumerate `/sys/class/hwmon/*` once, read
//! each node's `name`, and resolve the real file paths we care about (CPU temp,
//! fan RPM, NVMe temp, battery). Port of `scanner.py`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::sysfs;

pub const HWMON_ROOT: &str = "/sys/class/hwmon";
pub const KBD_LED: &str = "/sys/class/leds/asus::kbd_backlight";
pub const ACPI_PROFILE: &str = "/sys/firmware/acpi/platform_profile";

/// hwmon `name` values treated as a CPU package/die temperature, priority order.
const CPU_TEMP_CHIPS: &[&str] = &["k10temp", "zenpower", "coretemp", "cpu_thermal"];
/// Labels within a CPU chip that represent the headline package temperature.
const CPU_TEMP_LABELS: &[&str] = &["Tctl", "Tdie", "Package id 0", "Tccd1"];

#[derive(Debug, Clone)]
pub struct TempChannel {
    pub path: PathBuf,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct FanChannel {
    pub path: PathBuf,
    pub label: String,
}

/// Resolved, ready-to-read paths discovered once at startup.
#[derive(Debug, Default, Clone)]
pub struct HardwareMap {
    // Identity
    pub hostname: String,
    pub product: String,
    pub board: String,
    pub distro: String,
    pub kernel: String,

    // CPU
    pub cpu_temp: Option<TempChannel>,
    pub cpu_chip: String,

    // GPU
    pub gpu_vendor: String, // "nvidia" | "amd" | "intel" | "none"
    pub amd_gpu_hwmon: Option<PathBuf>,

    // Fans (ASUS platform fans)
    pub fans: Vec<FanChannel>,

    // Storage
    pub nvme_temp: Option<TempChannel>,

    // Battery / power supplies
    pub battery: Option<PathBuf>,
    pub ac_adapter: Option<PathBuf>,
    pub charge_limit_node: Option<PathBuf>,

    // Controls availability
    pub has_asusctl: bool,
    pub has_asusd: bool,
    pub has_platform_profile: bool,
    pub has_kbd_backlight: bool,
    pub kbd_max_brightness: i64,

    // Raw name -> hwmon dir, for diagnostics / --probe.
    pub hwmon_by_name: BTreeMap<String, String>,
}

impl HardwareMap {
    /// Compact human description used by the `--probe` text view.
    pub fn summary_lines(&self) -> Vec<String> {
        let gpu = if self.gpu_vendor != "none" {
            self.gpu_vendor.to_uppercase()
        } else {
            "none".to_string()
        };
        let cpu_label = self
            .cpu_temp
            .as_ref()
            .map(|c| c.label.clone())
            .unwrap_or_else(|| "n/a".into());
        let fans = if self.fans.is_empty() {
            "none".to_string()
        } else {
            self.fans
                .iter()
                .map(|f| f.label.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        vec![
            format!("host      : {}", self.hostname),
            format!("product   : {}", self.product),
            format!("board     : {}", self.board),
            format!("distro    : {}", self.distro),
            format!("kernel    : {}", self.kernel),
            format!(
                "cpu temp  : {} ({})",
                if self.cpu_chip.is_empty() { "?" } else { &self.cpu_chip },
                cpu_label
            ),
            format!("gpu       : {}", gpu),
            format!("fans      : {}", fans),
            format!("nvme temp : {}", if self.nvme_temp.is_some() { "yes" } else { "no" }),
            format!(
                "battery   : {}",
                self.battery
                    .as_ref()
                    .and_then(|b| b.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("none")
            ),
            format!(
                "chg limit : {}",
                if self.charge_limit_node.is_some() {
                    "writable via daemon"
                } else {
                    "n/a"
                }
            ),
            format!("asusctl   : {}   asusd: {}", self.has_asusctl, self.has_asusd),
            format!("profiles  : {}", self.has_platform_profile),
            format!(
                "kbd led   : {} (max {})",
                self.has_kbd_backlight, self.kbd_max_brightness
            ),
        ]
    }
}

fn detect_identity(hw: &mut HardwareMap) {
    hw.hostname = sysfs::read_text("/proc/sys/kernel/hostname").unwrap_or_default();
    hw.product = sysfs::read_text("/sys/class/dmi/id/product_name").unwrap_or_else(|| "unknown".into());
    hw.board = sysfs::read_text("/sys/class/dmi/id/board_name").unwrap_or_default();
    hw.kernel = sysfs::read_text("/proc/sys/kernel/osrelease").unwrap_or_default();
    // PRETTY_NAME from /etc/os-release without a distro crate.
    hw.distro = sysfs::read_text("/etc/os-release")
        .and_then(|txt| {
            txt.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into());
}

fn temp_channels(hwmon_dir: &Path) -> Vec<TempChannel> {
    sysfs::glob_in(hwmon_dir, "temp", "_input")
        .into_iter()
        .map(|inp| {
            let label_file = sibling(&inp, "_input", "_label");
            let label = sysfs::read_text(&label_file).unwrap_or_else(|| {
                inp.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("temp")
                    .replace("_input", "")
            });
            TempChannel { path: inp, label }
        })
        .collect()
}

/// Replace a filename suffix (`temp1_input` -> `temp1_label`).
fn sibling(path: &Path, from: &str, to: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .replace(from, to);
    path.with_file_name(name)
}

fn pick_cpu_temp(chips: &BTreeMap<String, PathBuf>) -> (String, Option<TempChannel>) {
    for chip in CPU_TEMP_CHIPS {
        let Some(dir) = chips.get(*chip) else { continue };
        let chans = temp_channels(dir);
        if chans.is_empty() {
            continue;
        }
        for wanted in CPU_TEMP_LABELS {
            if let Some(ch) = chans.iter().find(|c| c.label == *wanted) {
                return (chip.to_string(), Some(ch.clone()));
            }
        }
        return (chip.to_string(), Some(chans[0].clone()));
    }
    if let Some(dir) = chips.get("acpitz") {
        let chans = temp_channels(dir);
        if !chans.is_empty() {
            return ("acpitz".to_string(), Some(chans[0].clone()));
        }
    }
    (String::new(), None)
}

fn detect_gpu(hw: &mut HardwareMap, name_to_dir: &BTreeMap<String, PathBuf>) {
    if sysfs::which("nvidia-smi") {
        hw.gpu_vendor = "nvidia".into();
        return;
    }
    if let Some(dir) = name_to_dir.get("amdgpu") {
        hw.gpu_vendor = "amd".into();
        hw.amd_gpu_hwmon = Some(dir.clone());
        return;
    }
    if name_to_dir.contains_key("i915") || name_to_dir.contains_key("xe") {
        hw.gpu_vendor = "intel".into();
        return;
    }
    hw.gpu_vendor = "none".into();
}

fn detect_battery(hw: &mut HardwareMap) {
    let ps_root = Path::new("/sys/class/power_supply");
    if !ps_root.exists() {
        return;
    }
    for entry in sysfs::list_dir(ps_root) {
        let ps_type = sysfs::read_text(entry.join("type")).unwrap_or_default();
        let name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_uppercase();
        if ps_type == "Battery" || name.starts_with("BAT") {
            hw.battery = Some(entry.clone());
            let limit = entry.join("charge_control_end_threshold");
            if limit.exists() {
                hw.charge_limit_node = Some(limit);
            }
        } else if ps_type == "Mains"
            || name.starts_with("ACAD")
            || name.starts_with("AC")
            || name.starts_with("ADP")
        {
            hw.ac_adapter = Some(entry);
        }
    }
}

/// Discover the machine and return a fully-resolved `HardwareMap`.
pub fn scan() -> HardwareMap {
    let mut hw = HardwareMap::default();
    detect_identity(&mut hw);

    // 1. Enumerate every hwmon node by its declared name.
    let mut name_to_dir: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut cpu_chip_dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for node in sysfs::glob_in(HWMON_ROOT, "hwmon", "") {
        let Some(name) = sysfs::read_text(node.join("name")) else { continue };
        if name.is_empty() {
            continue;
        }
        name_to_dir.entry(name.clone()).or_insert_with(|| node.clone());
        hw.hwmon_by_name
            .entry(name.clone())
            .or_insert_with(|| node.to_string_lossy().to_string());
        if CPU_TEMP_CHIPS.contains(&name.as_str()) || name == "acpitz" {
            cpu_chip_dirs.entry(name).or_insert(node);
        }
    }

    // 2. CPU package temperature.
    let (chip, temp) = pick_cpu_temp(&cpu_chip_dirs);
    hw.cpu_chip = chip;
    hw.cpu_temp = temp;

    // 3. ASUS platform fans (name == "asus").
    if let Some(asus_dir) = name_to_dir.get("asus") {
        for inp in sysfs::glob_in(asus_dir, "fan", "_input") {
            let label_file = sibling(&inp, "_input", "_label");
            let label = sysfs::read_text(&label_file).unwrap_or_else(|| {
                inp.file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("fan")
                    .to_string()
            });
            hw.fans.push(FanChannel { path: inp, label });
        }
    }

    // 4. NVMe composite temperature.
    if let Some(nvme_dir) = name_to_dir.get("nvme") {
        let chans = temp_channels(nvme_dir);
        hw.nvme_temp = chans
            .iter()
            .find(|c| c.label.to_lowercase().starts_with("composite"))
            .cloned()
            .or_else(|| chans.first().cloned());
    }

    // 5. GPU vendor + battery.
    detect_gpu(&mut hw, &name_to_dir);
    detect_battery(&mut hw);

    // 6. Control surface availability.
    hw.has_asusctl = sysfs::which("asusctl");
    hw.has_asusd = sysfs::which("asusd");
    hw.has_platform_profile = Path::new(ACPI_PROFILE).exists();
    if Path::new(KBD_LED).exists() {
        hw.has_kbd_backlight = true;
        hw.kbd_max_brightness = sysfs::read_int(Path::new(KBD_LED).join("max_brightness")).unwrap_or(0);
    }

    hw
}

/// `--probe`: print the discovered hardware map and exit.
pub fn probe() -> Result<()> {
    let hw = scan();
    println!("=== armoury-tui hardware probe ===");
    for line in hw.summary_lines() {
        println!("  {line}");
    }
    println!("\n  hwmon nodes discovered:");
    for (name, path) in &hw.hwmon_by_name {
        println!("    {name:<28} {path}");
    }
    Ok(())
}
