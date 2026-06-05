//! ratatui draw helpers: formatters, neon palette, meters, sparklines, the
//! eighth-block area chart, and the per-tab panel builders. Port of `render.py`.
//!
//! The custom Text-based meters/sparklines/area chart are hand-rolled (identical
//! output to the Python original); ratatui's layout handles tab/panel placement.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::scanner::HardwareMap;
use crate::telemetry::Snapshot;

// -- palette (themed; backed by the global current theme in theme.rs) -------
#[inline] pub fn neon() -> Color { crate::theme::current().neon }
#[inline] pub fn cyan() -> Color { crate::theme::current().cyan }
#[inline] pub fn magenta() -> Color { crate::theme::current().magenta }
#[inline] pub fn amber() -> Color { crate::theme::current().amber }
#[inline] pub fn red() -> Color { crate::theme::current().red }
#[inline] pub fn dim() -> Color { crate::theme::current().dim }
#[inline] pub fn text() -> Color { crate::theme::current().text }
#[inline] pub fn blue() -> Color { crate::theme::current().blue }

/// Fan-RPM full-scale for the trend graph. Fixed (not auto-scaled) so a column's
/// vertical meaning is comparable from one moment to the next. ~6500 ≈ TUF A15.
pub const FAN_RPM_MAX: f64 = 6500.0;

const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const CHART_W: usize = 60;

// -- small formatters -------------------------------------------------------

/// Bytes -> human string (B/K/M/G/T), matching render.py:human_bytes.
pub fn human_bytes(n: f64) -> String {
    let mut n = n;
    for unit in ["B", "K", "M", "G", "T"] {
        if n.abs() < 1024.0 || unit == "T" {
            return if unit == "B" {
                format!("{:.0}{}", n, unit)
            } else {
                format!("{:.1}{}", n, unit)
            };
        }
        n /= 1024.0;
    }
    format!("{:.1}T", n)
}

/// Bytes/second -> human string (B/s, KB/s, ...), matching render.py:fmt_rate.
pub fn fmt_rate(bps: f64) -> String {
    let mut n = bps;
    for unit in ["B/s", "KB/s", "MB/s", "GB/s"] {
        if n.abs() < 1024.0 || unit == "GB/s" {
            return format!("{:5.1} {}", n, unit);
        }
        n /= 1024.0;
    }
    format!("{:.1} GB/s", n)
}

/// Pick a colour for a 0..1 fraction. `hot` = high is bad (temps / load).
pub fn grade(frac: f64, hot: bool) -> Color {
    if !hot {
        return cyan();
    }
    if frac >= 0.85 {
        red()
    } else if frac >= 0.6 {
        amber()
    } else {
        neon()
    }
}

// -- primitive widgets (return owned Spans/Lines) ---------------------------

/// A coloured horizontal bar: `████████──────────`.
fn meter(value: Option<f64>, maxv: f64, width: usize, hot: bool) -> Vec<Span<'static>> {
    let Some(value) = value else {
        return vec![Span::styled("  n/a", Style::new().fg(dim()))];
    };
    if maxv <= 0.0 {
        return vec![Span::styled("  n/a", Style::new().fg(dim()))];
    }
    let frac = (value / maxv).clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    vec![
        Span::styled("█".repeat(filled), Style::new().fg(grade(frac, hot))),
        Span::styled("─".repeat(width - filled), Style::new().fg(dim())),
    ]
}

/// Unicode-block sparkline of the most recent `width` samples.
fn sparkline(values: &[f64], width: usize, hot: bool) -> Vec<Span<'static>> {
    if values.is_empty() {
        return vec![Span::styled(BLOCKS[0].to_string().repeat(width), Style::new().fg(dim()))];
    }
    let data = &values[values.len().saturating_sub(width)..];
    let lo = data.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = if (hi - lo).abs() < f64::EPSILON { 1.0 } else { hi - lo };
    let mut out: Vec<Span<'static>> = Vec::new();
    for &v in data {
        let frac = (v - lo) / span;
        let idx = (frac * (BLOCKS.len() - 1) as f64) as usize;
        out.push(Span::styled(BLOCKS[idx.min(BLOCKS.len() - 1)].to_string(), Style::new().fg(grade(frac, hot))));
    }
    if data.len() < width {
        out.push(Span::styled(BLOCKS[0].to_string().repeat(width - data.len()), Style::new().fg(dim())));
    }
    out
}

/// A filled column/area graph of a time series, newest sample on the right.
fn area_chart(values: &[f64], maxv: f64, height: usize, color: Option<Color>, hot: bool) -> Vec<Line<'static>> {
    let width = CHART_W;
    let data = &values[values.len().saturating_sub(width)..];
    let maxv = if maxv <= 0.0 { 1.0 } else { maxv };
    let mut grid: Vec<Vec<(char, Color)>> = vec![vec![(' ', dim()); width]; height];
    let offset = width - data.len();
    for (i, &v) in data.iter().enumerate() {
        let col = offset + i;
        let frac = (v / maxv).clamp(0.0, 1.0);
        let filled = frac * height as f64;
        let full = filled as usize;
        let rem = filled - full as f64;
        let cell = color.unwrap_or_else(|| grade(frac, hot));
        for r in 0..height {
            let level = height - 1 - r;
            if level < full {
                grid[r][col] = ('█', cell);
            } else if level == full && rem > 1e-6 {
                let idx = ((rem * 8.0) as usize).max(1).min(8);
                grid[r][col] = (BLOCKS[idx], cell);
            }
        }
    }
    grid.into_iter()
        .map(|row| Line::from(row.into_iter().map(|(c, st)| Span::styled(c.to_string(), Style::new().fg(st))).collect::<Vec<_>>()))
        .collect()
}

fn axis() -> Line<'static> {
    Line::styled(format!("└{}", "─".repeat(CHART_W - 1)), Style::new().fg(dim()))
}

/// `label:value` key/value line with a fixed-width dim label.
fn kv(label: &str, value: &str, vstyle: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<9}", label), Style::new().fg(dim())),
        Span::styled(value.to_string(), Style::new().fg(vstyle)),
    ])
}

/// Wrap body text in a titled, coloured border (the standard panel chrome).
pub fn panel(body: Text<'static>, title: &str, color: Color) -> Paragraph<'static> {
    let block = Block::bordered()
        .title(Span::styled(format!(" {title} "), Style::new().fg(color).add_modifier(Modifier::BOLD)))
        .border_style(Style::new().fg(color));
    Paragraph::new(body).block(block)
}

// -- subsystem panels -------------------------------------------------------

pub fn cpu_panel(s: &Snapshot, hist: &[f64]) -> Paragraph<'static> {
    let c = &s.cpu;
    let mut lines: Vec<Line> = Vec::new();
    let mut usage = vec![Span::styled(format!("{:<9}", "usage"), Style::new().fg(dim())),
                         Span::styled(format!("{:5.1}%  ", c.overall), Style::new().fg(cyan()))];
    usage.extend(meter(Some(c.overall), 100.0, 18, true));
    lines.push(Line::from(usage));

    let temp = c.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "n/a".into());
    let tstyle = grade(c.temp_c.unwrap_or(0.0) / 95.0, true);
    let avg_freq = if c.freqs_mhz.is_empty() { 0.0 } else { c.freqs_mhz.iter().sum::<f64>() / c.freqs_mhz.len() as f64 };
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "temp"), Style::new().fg(dim())),
        Span::styled(format!("{:<8}", temp), Style::new().fg(tstyle)),
        Span::styled(format!("{:<9}", "clock"), Style::new().fg(dim())),
        Span::styled(format!("{:.2} GHz", avg_freq / 1000.0), Style::new().fg(blue())),
    ]));
    let load = c.load1.map(|l| format!("{l:.2}")).unwrap_or_else(|| "n/a".into());
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "load1"), Style::new().fg(dim())),
        Span::styled(format!("{:<8}", load), Style::new().fg(text())),
        Span::styled(format!("{:<9}", "cores"), Style::new().fg(dim())),
        Span::styled(format!("{}", c.cores), Style::new().fg(text())),
    ]));
    lines.push(Line::from(""));
    // per-core block strip
    let mut strip = vec![Span::styled("cores  ", Style::new().fg(dim()))];
    for &u in &c.per_core {
        let idx = (u / 100.0 * (BLOCKS.len() - 1) as f64) as usize;
        strip.push(Span::styled(BLOCKS[idx.min(BLOCKS.len() - 1)].to_string(), Style::new().fg(grade(u / 100.0, true))));
    }
    lines.push(Line::from(strip));
    lines.push(Line::from(""));
    let mut h = vec![Span::styled("history ", Style::new().fg(dim()))];
    h.extend(sparkline(hist, 28, true));
    lines.push(Line::from(h));
    panel(Text::from(lines), "CPU", neon())
}

pub fn gpu_panel(s: &Snapshot, hist: &[f64]) -> Paragraph<'static> {
    let g = &s.gpu;
    if !g.present {
        return panel(Text::styled(format!("no readable GPU ({})", g.vendor), Style::new().fg(dim())), "GPU", magenta());
    }
    let mut lines: Vec<Line> = Vec::new();
    let util = g.util.unwrap_or(0.0);
    let mut u = vec![Span::styled(format!("{:<9}", "usage"), Style::new().fg(dim())),
                     Span::styled(format!("{:5.1}%  ", util), Style::new().fg(magenta()))];
    u.extend(meter(Some(util), 100.0, 18, false));
    lines.push(Line::from(u));
    let temp = g.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "n/a".into());
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "temp"), Style::new().fg(dim())),
        Span::styled(format!("{:<8}", temp), Style::new().fg(grade(g.temp_c.unwrap_or(0.0) / 90.0, true))),
        Span::styled(format!("{:<9}", "power"), Style::new().fg(dim())),
        Span::styled(g.power_w.map(|p| format!("{p:.1} W")).unwrap_or_else(|| "n/a".into()), Style::new().fg(amber())),
    ]));
    if let Some(total) = g.mem_total_mb {
        let used = g.mem_used_mb.unwrap_or(0.0);
        let mut m = vec![Span::styled(format!("{:<9}", "vram"), Style::new().fg(dim())),
                         Span::styled(format!("{:.0}/{:.0} MB  ", used, total), Style::new().fg(cyan()))];
        m.extend(meter(Some(used), total, 14, false));
        lines.push(Line::from(m));
    }
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "clock"), Style::new().fg(dim())),
        Span::styled(format!("{:<11}", g.clock_mhz.map(|c| format!("{c:.0} MHz")).unwrap_or_else(|| "n/a".into())), Style::new().fg(blue())),
        Span::styled(format!("{:<9}", "fan"), Style::new().fg(dim())),
        Span::styled(g.fan_pct.map(|f| format!("{f:.0}%")).unwrap_or_else(|| "n/a".into()), Style::new().fg(text())),
    ]));
    lines.push(Line::styled(g.name.clone(), Style::new().fg(dim())));
    lines.push(Line::from(""));
    let mut h = vec![Span::styled("history ", Style::new().fg(dim()))];
    h.extend(sparkline(hist, 28, false));
    lines.push(Line::from(h));
    panel(Text::from(lines), "GPU", magenta())
}

pub fn mem_panel(s: &Snapshot) -> Paragraph<'static> {
    let m = &s.mem;
    let mut lines: Vec<Line> = Vec::new();
    let mut r = vec![Span::styled(format!("{:<9}", "ram"), Style::new().fg(dim())),
                     Span::styled(format!("{:4.1}%  ", m.percent), Style::new().fg(cyan()))];
    r.extend(meter(Some(m.percent), 100.0, 16, true));
    lines.push(Line::from(r));
    lines.push(kv("", &format!("{} / {}", human_bytes(m.used as f64), human_bytes(m.total as f64)), text()));
    if m.swap_total > 0 {
        let swp = m.swap_used as f64 / m.swap_total as f64 * 100.0;
        let mut sr = vec![Span::styled(format!("{:<9}", "swap"), Style::new().fg(dim())),
                          Span::styled(format!("{:4.1}%  ", swp), Style::new().fg(text()))];
        sr.extend(meter(Some(swp), 100.0, 16, true));
        lines.push(Line::from(sr));
    }
    panel(Text::from(lines), "MEMORY", cyan())
}

pub fn fan_panel(s: &Snapshot, hist: &std::collections::HashMap<String, Vec<f64>>) -> Paragraph<'static> {
    if s.fans.is_empty() {
        return panel(Text::styled("no ASUS fan channels", Style::new().fg(dim())), "FANS", blue());
    }
    let mut lines: Vec<Line> = Vec::new();
    for f in &s.fans {
        let empty = Vec::new();
        let h = hist.get(&f.label).unwrap_or(&empty);
        let mut row = vec![
            Span::styled(format!("{:<10}", f.label.replace('_', " ")), Style::new().fg(blue())),
            Span::styled(format!("{:>5} rpm  ", f.rpm), Style::new().fg(if f.rpm > 0 { neon() } else { dim() })),
        ];
        row.extend(sparkline(h, 16, true));
        lines.push(Line::from(row));
    }
    panel(Text::from(lines), "FANS", blue())
}

pub fn battery_panel(s: &Snapshot) -> Paragraph<'static> {
    let b = &s.battery;
    if !b.present {
        return panel(Text::styled("no battery", Style::new().fg(dim())), "BATTERY", amber());
    }
    let pct = b.percent.unwrap_or(0.0);
    let pcol = if pct < 20.0 { red() } else if pct < 40.0 { amber() } else { neon() };
    let mut lines: Vec<Line> = Vec::new();
    let mut r = vec![Span::styled(format!("{:<9}", "charge"), Style::new().fg(dim())),
                     Span::styled(format!("{:4.0}%  ", pct), Style::new().fg(pcol))];
    r.extend(meter(Some(pct), 100.0, 16, false));
    lines.push(Line::from(r));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "state"), Style::new().fg(dim())),
        Span::styled(format!("{:<16}", b.status), Style::new().fg(cyan())),
        Span::styled(format!("{:<9}", "ac"), Style::new().fg(dim())),
        Span::styled(if b.ac_online == Some(true) { "online" } else { "offline" },
                     Style::new().fg(if b.ac_online == Some(true) { neon() } else { dim() })),
    ]));
    if let Some(rate) = b.rate_w {
        let arrow = if rate > 0.0 { "▲" } else if rate < 0.0 { "▼" } else { "•" };
        let rcol = if rate > 0.0 { neon() } else { amber() };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<9}", "power"), Style::new().fg(dim())),
            Span::styled(format!("{} {:.1} W   ", arrow, rate.abs()), Style::new().fg(rcol)),
            Span::styled(format!("{:<9}", "limit"), Style::new().fg(dim())),
            Span::styled(b.charge_limit.map(|l| format!("{l}%")).unwrap_or_else(|| "—".into()), Style::new().fg(magenta())),
        ]));
    }
    // Health (full/design), cycle count, and estimated time-remaining.
    let mut hl: Vec<Span> = Vec::new();
    if let Some(h) = b.health_pct {
        let hcol = if h >= 80.0 { neon() } else if h >= 60.0 { amber() } else { red() };
        hl.push(Span::styled(format!("{:<9}", "health"), Style::new().fg(dim())));
        hl.push(Span::styled(format!("{h:.0}%  "), Style::new().fg(hcol)));
    }
    if let Some(c) = b.cycle_count.filter(|&c| c > 0) {
        hl.push(Span::styled(format!("{:<8}", "cycles"), Style::new().fg(dim())));
        hl.push(Span::styled(format!("{c}"), Style::new().fg(text())));
    }
    if !hl.is_empty() {
        lines.push(Line::from(hl));
    }
    let charging = b.status.eq_ignore_ascii_case("charging");
    if let Some(secs) = if charging { b.time_to_full_s } else { b.time_to_empty_s } {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<9}", if charging { "to full" } else { "remaining" }), Style::new().fg(dim())),
            Span::styled(format!("{}h {}m", secs / 3600, (secs % 3600) / 60), Style::new().fg(cyan())),
        ]));
    }
    panel(Text::from(lines), "BATTERY", amber())
}

pub fn storage_panel(s: &Snapshot) -> Paragraph<'static> {
    let st = &s.storage;
    let nvme = st.nvme_temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "n/a".into());
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "nvme"), Style::new().fg(dim())),
        Span::styled(format!("{:<8}", nvme), Style::new().fg(grade(st.nvme_temp_c.unwrap_or(0.0) / 80.0, true))),
        Span::styled(format!("{:<9}", "root"), Style::new().fg(dim())),
        Span::styled(format!("{:.0}%", st.root_percent), Style::new().fg(cyan())),
    ]));
    let mut r = vec![Span::styled(format!("{:<9}", "disk"), Style::new().fg(dim())),
                     Span::styled(format!("{} / {}  ", human_bytes(st.root_used as f64), human_bytes(st.root_total as f64)), Style::new().fg(text()))];
    r.extend(meter(Some(st.root_percent), 100.0, 12, false));
    lines.push(Line::from(r));
    panel(Text::from(lines), "STORAGE", blue())
}

pub fn system_panel(hw: &HardwareMap, s: &Snapshot) -> Paragraph<'static> {
    let up = s.uptime_s as u64;
    let (d, rem) = (up / 86400, up % 86400);
    let (h, rem) = (rem / 3600, rem % 3600);
    let uptime = format!("{}{}h {}m", if d > 0 { format!("{d}d ") } else { String::new() }, h, rem / 60);
    let lines = vec![
        kv("host", &hw.hostname, neon()),
        kv("model", &hw.product, text()),
        kv("distro", &hw.distro, text()),
        kv("kernel", &hw.kernel, dim()),
        kv("uptime", &uptime, cyan()),
    ];
    panel(Text::from(lines), "SYSTEM", neon())
}

pub fn profile_banner(s: &Snapshot) -> Paragraph<'static> {
    let prof = s.profile.clone().unwrap_or_else(|| "unknown".into());
    let (icon, color) = match prof.as_str() {
        "Quiet" => ("🌙", cyan()),
        "Balanced" => ("⚖", neon()),
        "Performance" => ("🚀", magenta()),
        _ => ("•", text()),
    };
    let body = Text::from(Line::styled(format!("{}  {}", icon, prof.to_uppercase()),
                                       Style::new().fg(color).add_modifier(Modifier::BOLD)));
    panel(body, "ACTIVE PROFILE", color)
}

pub fn headline_strip(s: &Snapshot) -> Paragraph<'static> {
    let c = &s.cpu;
    let g = &s.gpu;
    let mut tiles: Vec<(String, &'static str, Color)> = Vec::new();
    tiles.push((c.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "—".into()), "CPU TEMP", grade(c.temp_c.unwrap_or(0.0) / 95.0, true)));
    tiles.push((format!("{:.0}%", c.overall), "CPU LOAD", grade(c.overall / 100.0, true)));
    if g.present {
        tiles.push((g.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "—".into()), "GPU TEMP", grade(g.temp_c.unwrap_or(0.0) / 90.0, true)));
        tiles.push((g.util.map(|u| format!("{u:.0}%")).unwrap_or_else(|| "—".into()), "GPU LOAD", grade(g.util.unwrap_or(0.0) / 100.0, true)));
    }
    let fmax = s.fans.iter().map(|f| f.rpm).max().unwrap_or(0);
    tiles.push((format!("{fmax}"), "FAN RPM", grade(fmax as f64 / FAN_RPM_MAX, true)));
    let b = &s.battery;
    if b.present && b.rate_w.map(|r| r < 0.0).unwrap_or(false) {
        tiles.push((format!("{:.0}W", b.rate_w.unwrap().abs()), "SYS DRAW", amber()));
    } else if g.present && g.power_w.is_some() {
        tiles.push((format!("{:.0}W", g.power_w.unwrap()), "GPU PWR", amber()));
    } else {
        tiles.push((format!("{:.0}%", s.mem.percent), "RAM", cyan()));
    }

    let cell = 14usize;
    let vals: Vec<Span> = tiles.iter().map(|(v, _, c)| Span::styled(format!("{:^width$}", v, width = cell), Style::new().fg(*c).add_modifier(Modifier::BOLD))).collect();
    let labs: Vec<Span> = tiles.iter().map(|(_, l, _)| Span::styled(format!("{:^width$}", l, width = cell), Style::new().fg(dim()))).collect();
    panel(Text::from(vec![Line::from(vals), Line::from(labs)]), "SYSTEM AT A GLANCE", neon())
}

pub fn thermal_graph_panel(s: &Snapshot, cpu_hist: &[f64], gpu_hist: &[f64]) -> Paragraph<'static> {
    let mut lines: Vec<Line> = Vec::new();
    let ct = s.cpu.temp_c;
    lines.push(Line::from(vec![
        Span::styled("CPU °C", Style::new().fg(cyan())),
        Span::styled(ct.map(|t| format!("   {t:.0}°C")).unwrap_or_else(|| "   —".into()), Style::new().fg(grade(ct.unwrap_or(0.0) / 95.0, true))),
        Span::styled("      scale 0–100°C", Style::new().fg(dim())),
    ]));
    let cdata = if cpu_hist.is_empty() { vec![ct.unwrap_or(0.0)] } else { cpu_hist.to_vec() };
    lines.extend(area_chart(&cdata, 100.0, 5, None, true));
    lines.push(axis());
    if s.gpu.present {
        let gt = s.gpu.temp_c;
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("GPU °C", Style::new().fg(magenta())),
            Span::styled(gt.map(|t| format!("   {t:.0}°C")).unwrap_or_else(|| "   —".into()), Style::new().fg(grade(gt.unwrap_or(0.0) / 90.0, true))),
            Span::styled("      scale 0–100°C", Style::new().fg(dim())),
        ]));
        let gdata = if gpu_hist.is_empty() { vec![gt.unwrap_or(0.0)] } else { gpu_hist.to_vec() };
        lines.extend(area_chart(&gdata, 100.0, 5, None, true));
        lines.push(axis());
    }
    panel(Text::from(lines), "THERMAL TREND  (live)", red())
}

pub fn fan_graph_panel(s: &Snapshot, hist: &std::collections::HashMap<String, Vec<f64>>) -> Paragraph<'static> {
    if s.fans.is_empty() {
        return panel(Text::styled("no ASUS fan channels", Style::new().fg(dim())), "FAN SPEED", blue());
    }
    let mut lines: Vec<Line> = Vec::new();
    for (n, f) in s.fans.iter().enumerate() {
        if n > 0 {
            lines.push(Line::from(""));
        }
        let frac = (f.rpm as f64 / FAN_RPM_MAX).min(1.0);
        lines.push(Line::from(vec![
            Span::styled(f.label.replace('_', " ").to_uppercase(), Style::new().fg(cyan())),
            Span::styled(format!("   {} ", f.rpm), Style::new().fg(if f.rpm > 0 { grade(frac, true) } else { dim() })),
            Span::styled("rpm", Style::new().fg(dim())),
            Span::styled(format!("      scale 0–{} rpm", FAN_RPM_MAX as i64), Style::new().fg(dim())),
        ]));
        let series = hist.get(&f.label).cloned().unwrap_or_else(|| vec![f.rpm as f64]);
        lines.extend(area_chart(&series, FAN_RPM_MAX, 7, None, true));
        lines.push(axis());
    }
    panel(Text::from(lines), "FAN SPEED  (live trend)", blue())
}

/// The temp→fan% points of a profile's CPU/GPU curves, as a readable shape.
pub fn fan_curve_panel(curves: &[crate::control::FanCurve], profile: &str, active: bool) -> Paragraph<'static> {
    let title = format!("FAN CURVE · {profile}");
    if curves.is_empty() {
        return panel(Text::styled("no fan-curve data — needs asusd", Style::new().fg(dim())), &title, amber());
    }
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(profile.to_uppercase(), Style::new().fg(magenta()).add_modifier(Modifier::BOLD)),
        Span::styled(
            if active { "  (active — drives the fans now)" } else { "  (edit only — not the live profile)" },
            Style::new().fg(if active { neon() } else { dim() }),
        ),
    ]));
    for c in curves {
        let pcts = c.pwm_pcts();
        lines.push(Line::from(vec![
            Span::styled(format!("{:<4}", c.fan), Style::new().fg(cyan())),
            Span::styled(
                if c.enabled { "curve enabled" } else { "curve disabled (firmware default)" },
                Style::new().fg(if c.enabled { neon() } else { dim() }),
            ),
        ]));
        let temps: String = c.points.iter().map(|&(t, _)| format!("{t:>3}")).collect::<Vec<_>>().join(" ");
        lines.push(Line::from(vec![
            Span::styled("  temp ", Style::new().fg(dim())),
            Span::styled(temps, Style::new().fg(text())),
            Span::styled(" °C", Style::new().fg(dim())),
        ]));
        let mut fl = vec![Span::styled("  fan  ", Style::new().fg(dim()))];
        for p in &pcts {
            fl.push(Span::styled(format!("{p:>3} "), Style::new().fg(grade(*p as f64 / 100.0, true))));
        }
        fl.push(Span::styled("%", Style::new().fg(dim())));
        lines.push(Line::from(fl));
    }
    panel(Text::from(lines), &title, magenta())
}

/// Detail card for the currently-selected process (Phase C).
pub fn proc_detail_panel(d: Option<&crate::telemetry::ProcDetail>) -> Paragraph<'static> {
    let Some(d) = d else {
        return panel(Text::styled("select a process to inspect", Style::new().fg(dim())), "PROCESS DETAIL", cyan());
    };
    let kv2 = |k: &str, v: String, vc: Color| {
        Line::from(vec![
            Span::styled(format!("{:>9} ", k), Style::new().fg(dim())),
            Span::styled(v, Style::new().fg(vc)),
        ])
    };
    // start_time → UTC HH:MM:SS without pulling in a date crate.
    let sod = d.start_time % 86_400;
    let started = format!("{:02}:{:02}:{:02} UTC", sod / 3600, (sod % 3600) / 60, sod % 60);
    let status_col = if d.status == "run" || d.status == "sleep" || d.status == "running" || d.status == "sleeping" {
        text()
    } else {
        amber()
    };
    let lines = vec![
        kv2("pid", d.pid.to_string(), neon()),
        kv2("name", d.name.clone(), cyan()),
        kv2("status", d.status.clone(), status_col),
        kv2("user", d.user.clone(), text()),
        kv2("ppid", d.ppid.map(|p| p.to_string()).unwrap_or_else(|| "—".into()), text()),
        kv2("cpu", format!("{:.1}%", d.cpu), grade((d.cpu.min(100.0)) / 100.0, true)),
        kv2("memory", format!("{:.0} MB", d.mem_mb), cyan()),
        kv2("started", started, text()),
        kv2("cmd", d.cmd.chars().take(120).collect::<String>(), dim()),
    ];
    panel(Text::from(lines), "PROCESS DETAIL", cyan())
}

pub fn gpu_proc_panel(s: &Snapshot) -> Paragraph<'static> {
    if !s.gpu.present {
        return panel(Text::styled("no GPU", Style::new().fg(dim())), "GPU PROCESSES", magenta());
    }
    if s.gpu_procs.is_empty() {
        return panel(Text::styled("no GPU compute processes", Style::new().fg(dim())), "GPU PROCESSES", magenta());
    }
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(format!("{:<7}", "pid"), Style::new().fg(dim())),
        Span::styled(format!("{:<28}", "process"), Style::new().fg(dim())),
        Span::styled(format!("{:>8}", "vram"), Style::new().fg(dim())),
    ])];
    for p in &s.gpu_procs {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<7}", p.pid), Style::new().fg(dim())),
            Span::styled(format!("{:<28}", p.name.chars().take(27).collect::<String>()), Style::new().fg(text())),
            Span::styled(format!("{:>6.0}M", p.mem_mb), Style::new().fg(magenta())),
        ]));
    }
    panel(Text::from(lines), "GPU PROCESSES", magenta())
}

pub fn net_table(s: &Snapshot) -> Paragraph<'static> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(format!("{:<14}", "iface"), Style::new().fg(dim())),
        Span::styled(format!("{:<6}", "state"), Style::new().fg(dim())),
        Span::styled(format!("{:<16}", "IPv4"), Style::new().fg(dim())),
        Span::styled(format!("{:>11}", "▼ down"), Style::new().fg(dim())),
        Span::styled(format!("{:>11}", "▲ up"), Style::new().fg(dim())),
        Span::styled(format!("{:>9}", "RX"), Style::new().fg(dim())),
        Span::styled(format!("{:>9}", "TX"), Style::new().fg(dim())),
        Span::styled(format!("{:>9}", "err/drop"), Style::new().fg(dim())),
    ]));
    let shown: Vec<_> = s.net.iter().filter(|i| !i.is_virtual || i.up_bps + i.down_bps > 0.0).take(10).collect();
    for i in shown {
        let errs = i.errin + i.errout + i.dropin + i.dropout;
        lines.push(Line::from(vec![
            Span::styled(format!("{:<14}", format!("{}{}", i.name, if i.is_virtual { " *" } else { "" })),
                         Style::new().fg(if i.is_virtual { dim() } else { cyan() })),
            Span::styled(format!("{:<6}", if i.is_up { "UP" } else { "down" }),
                         Style::new().fg(if i.is_up { neon() } else { dim() })),
            Span::styled(format!("{:<16}", if i.ipv4.is_empty() { "—" } else { &i.ipv4 }), Style::new().fg(text())),
            Span::styled(format!("{:>11}", fmt_rate(i.down_bps).trim()), Style::new().fg(neon())),
            Span::styled(format!("{:>11}", fmt_rate(i.up_bps).trim()), Style::new().fg(amber())),
            Span::styled(format!("{:>9}", human_bytes(i.rx_total as f64)), Style::new().fg(dim())),
            Span::styled(format!("{:>9}", human_bytes(i.tx_total as f64)), Style::new().fg(dim())),
            Span::styled(format!("{:>9}", errs), Style::new().fg(if errs > 0 { red() } else { dim() })),
        ]));
    }
    panel(Text::from(lines), "INTERFACES  (* = virtual)", cyan())
}

pub fn bandwidth_graph_panel(down_hist: &[f64], up_hist: &[f64], cur_down: f64, cur_up: f64) -> Paragraph<'static> {
    let dmax = down_hist.iter().cloned().fold(0.0_f64, f64::max).max(1024.0);
    let umax = up_hist.iter().cloned().fold(0.0_f64, f64::max).max(1024.0);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("▼ DOWN  ", Style::new().fg(neon())),
        Span::styled(fmt_rate(cur_down).trim().to_string(), Style::new().fg(neon())),
        Span::styled(format!("      peak {}", fmt_rate(dmax).trim()), Style::new().fg(dim())),
    ]));
    let d = if down_hist.is_empty() { vec![0.0] } else { down_hist.to_vec() };
    lines.extend(area_chart(&d, dmax, 5, Some(neon()), true));
    lines.push(axis());
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("▲ UP    ", Style::new().fg(amber())),
        Span::styled(fmt_rate(cur_up).trim().to_string(), Style::new().fg(amber())),
        Span::styled(format!("      peak {}", fmt_rate(umax).trim()), Style::new().fg(dim())),
    ]));
    let u = if up_hist.is_empty() { vec![0.0] } else { up_hist.to_vec() };
    lines.extend(area_chart(&u, umax, 5, Some(amber()), true));
    lines.push(axis());
    panel(Text::from(lines), "BANDWIDTH  (live trend)", cyan())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_units() {
        assert_eq!(human_bytes(512.0), "512B");
        assert_eq!(human_bytes(1536.0), "1.5K");
        assert_eq!(human_bytes(1024.0 * 1024.0 * 3.0), "3.0M");
    }

    #[test]
    fn rate_units() {
        assert_eq!(fmt_rate(0.0).trim(), "0.0 B/s");
        assert_eq!(fmt_rate(2048.0).trim(), "2.0 KB/s");
    }

    #[test]
    fn grade_thresholds() {
        assert_eq!(grade(0.5, true), neon());
        assert_eq!(grade(0.7, true), amber());
        assert_eq!(grade(0.9, true), red());
        assert_eq!(grade(0.99, false), cyan());
    }

    #[test]
    fn area_chart_dims() {
        let rows = area_chart(&[1.0, 2.0, 3.0], 3.0, 5, None, true);
        assert_eq!(rows.len(), 5);
    }
}
