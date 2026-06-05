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

// -- neon palette -----------------------------------------------------------
pub const NEON_GREEN: Color = Color::Rgb(0x00, 0xff, 0x9c);
pub const CYAN: Color = Color::Rgb(0x36, 0xf9, 0xf6);
pub const MAGENTA: Color = Color::Rgb(0xff, 0x2e, 0x88);
pub const AMBER: Color = Color::Rgb(0xf3, 0xd0, 0x00);
pub const RED: Color = Color::Rgb(0xff, 0x33, 0x55);
pub const DIM: Color = Color::Rgb(0x4b, 0x52, 0x63);
pub const TEXT: Color = Color::Rgb(0xc9, 0xd1, 0xd9);
pub const BLUE: Color = Color::Rgb(0x5a, 0xc8, 0xfa);

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
        return CYAN;
    }
    if frac >= 0.85 {
        RED
    } else if frac >= 0.6 {
        AMBER
    } else {
        NEON_GREEN
    }
}

// -- primitive widgets (return owned Spans/Lines) ---------------------------

/// A coloured horizontal bar: `████████──────────`.
fn meter(value: Option<f64>, maxv: f64, width: usize, hot: bool) -> Vec<Span<'static>> {
    let Some(value) = value else {
        return vec![Span::styled("  n/a", Style::new().fg(DIM))];
    };
    if maxv <= 0.0 {
        return vec![Span::styled("  n/a", Style::new().fg(DIM))];
    }
    let frac = (value / maxv).clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    vec![
        Span::styled("█".repeat(filled), Style::new().fg(grade(frac, hot))),
        Span::styled("─".repeat(width - filled), Style::new().fg(DIM)),
    ]
}

/// Unicode-block sparkline of the most recent `width` samples.
fn sparkline(values: &[f64], width: usize, hot: bool) -> Vec<Span<'static>> {
    if values.is_empty() {
        return vec![Span::styled(BLOCKS[0].to_string().repeat(width), Style::new().fg(DIM))];
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
        out.push(Span::styled(BLOCKS[0].to_string().repeat(width - data.len()), Style::new().fg(DIM)));
    }
    out
}

/// A filled column/area graph of a time series, newest sample on the right.
fn area_chart(values: &[f64], maxv: f64, height: usize, color: Option<Color>, hot: bool) -> Vec<Line<'static>> {
    let width = CHART_W;
    let data = &values[values.len().saturating_sub(width)..];
    let maxv = if maxv <= 0.0 { 1.0 } else { maxv };
    let mut grid: Vec<Vec<(char, Color)>> = vec![vec![(' ', DIM); width]; height];
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
    Line::styled(format!("└{}", "─".repeat(CHART_W - 1)), Style::new().fg(DIM))
}

/// `label:value` key/value line with a fixed-width dim label.
fn kv(label: &str, value: &str, vstyle: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<9}", label), Style::new().fg(DIM)),
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
    let mut usage = vec![Span::styled(format!("{:<9}", "usage"), Style::new().fg(DIM)),
                         Span::styled(format!("{:5.1}%  ", c.overall), Style::new().fg(CYAN))];
    usage.extend(meter(Some(c.overall), 100.0, 18, true));
    lines.push(Line::from(usage));

    let temp = c.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "n/a".into());
    let tstyle = grade(c.temp_c.unwrap_or(0.0) / 95.0, true);
    let avg_freq = if c.freqs_mhz.is_empty() { 0.0 } else { c.freqs_mhz.iter().sum::<f64>() / c.freqs_mhz.len() as f64 };
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "temp"), Style::new().fg(DIM)),
        Span::styled(format!("{:<8}", temp), Style::new().fg(tstyle)),
        Span::styled(format!("{:<9}", "clock"), Style::new().fg(DIM)),
        Span::styled(format!("{:.2} GHz", avg_freq / 1000.0), Style::new().fg(BLUE)),
    ]));
    let load = c.load1.map(|l| format!("{l:.2}")).unwrap_or_else(|| "n/a".into());
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "load1"), Style::new().fg(DIM)),
        Span::styled(format!("{:<8}", load), Style::new().fg(TEXT)),
        Span::styled(format!("{:<9}", "cores"), Style::new().fg(DIM)),
        Span::styled(format!("{}", c.cores), Style::new().fg(TEXT)),
    ]));
    lines.push(Line::from(""));
    // per-core block strip
    let mut strip = vec![Span::styled("cores  ", Style::new().fg(DIM))];
    for &u in &c.per_core {
        let idx = (u / 100.0 * (BLOCKS.len() - 1) as f64) as usize;
        strip.push(Span::styled(BLOCKS[idx.min(BLOCKS.len() - 1)].to_string(), Style::new().fg(grade(u / 100.0, true))));
    }
    lines.push(Line::from(strip));
    lines.push(Line::from(""));
    let mut h = vec![Span::styled("history ", Style::new().fg(DIM))];
    h.extend(sparkline(hist, 28, true));
    lines.push(Line::from(h));
    panel(Text::from(lines), "CPU", NEON_GREEN)
}

pub fn gpu_panel(s: &Snapshot, hist: &[f64]) -> Paragraph<'static> {
    let g = &s.gpu;
    if !g.present {
        return panel(Text::styled(format!("no readable GPU ({})", g.vendor), Style::new().fg(DIM)), "GPU", MAGENTA);
    }
    let mut lines: Vec<Line> = Vec::new();
    let util = g.util.unwrap_or(0.0);
    let mut u = vec![Span::styled(format!("{:<9}", "usage"), Style::new().fg(DIM)),
                     Span::styled(format!("{:5.1}%  ", util), Style::new().fg(MAGENTA))];
    u.extend(meter(Some(util), 100.0, 18, false));
    lines.push(Line::from(u));
    let temp = g.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "n/a".into());
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "temp"), Style::new().fg(DIM)),
        Span::styled(format!("{:<8}", temp), Style::new().fg(grade(g.temp_c.unwrap_or(0.0) / 90.0, true))),
        Span::styled(format!("{:<9}", "power"), Style::new().fg(DIM)),
        Span::styled(g.power_w.map(|p| format!("{p:.1} W")).unwrap_or_else(|| "n/a".into()), Style::new().fg(AMBER)),
    ]));
    if let Some(total) = g.mem_total_mb {
        let used = g.mem_used_mb.unwrap_or(0.0);
        let mut m = vec![Span::styled(format!("{:<9}", "vram"), Style::new().fg(DIM)),
                         Span::styled(format!("{:.0}/{:.0} MB  ", used, total), Style::new().fg(CYAN))];
        m.extend(meter(Some(used), total, 14, false));
        lines.push(Line::from(m));
    }
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "clock"), Style::new().fg(DIM)),
        Span::styled(format!("{:<11}", g.clock_mhz.map(|c| format!("{c:.0} MHz")).unwrap_or_else(|| "n/a".into())), Style::new().fg(BLUE)),
        Span::styled(format!("{:<9}", "fan"), Style::new().fg(DIM)),
        Span::styled(g.fan_pct.map(|f| format!("{f:.0}%")).unwrap_or_else(|| "n/a".into()), Style::new().fg(TEXT)),
    ]));
    lines.push(Line::styled(g.name.clone(), Style::new().fg(DIM)));
    lines.push(Line::from(""));
    let mut h = vec![Span::styled("history ", Style::new().fg(DIM))];
    h.extend(sparkline(hist, 28, false));
    lines.push(Line::from(h));
    panel(Text::from(lines), "GPU", MAGENTA)
}

pub fn mem_panel(s: &Snapshot) -> Paragraph<'static> {
    let m = &s.mem;
    let mut lines: Vec<Line> = Vec::new();
    let mut r = vec![Span::styled(format!("{:<9}", "ram"), Style::new().fg(DIM)),
                     Span::styled(format!("{:4.1}%  ", m.percent), Style::new().fg(CYAN))];
    r.extend(meter(Some(m.percent), 100.0, 16, true));
    lines.push(Line::from(r));
    lines.push(kv("", &format!("{} / {}", human_bytes(m.used as f64), human_bytes(m.total as f64)), TEXT));
    if m.swap_total > 0 {
        let swp = m.swap_used as f64 / m.swap_total as f64 * 100.0;
        let mut sr = vec![Span::styled(format!("{:<9}", "swap"), Style::new().fg(DIM)),
                          Span::styled(format!("{:4.1}%  ", swp), Style::new().fg(TEXT))];
        sr.extend(meter(Some(swp), 100.0, 16, true));
        lines.push(Line::from(sr));
    }
    panel(Text::from(lines), "MEMORY", CYAN)
}

pub fn fan_panel(s: &Snapshot, hist: &std::collections::HashMap<String, Vec<f64>>) -> Paragraph<'static> {
    if s.fans.is_empty() {
        return panel(Text::styled("no ASUS fan channels", Style::new().fg(DIM)), "FANS", BLUE);
    }
    let mut lines: Vec<Line> = Vec::new();
    for f in &s.fans {
        let empty = Vec::new();
        let h = hist.get(&f.label).unwrap_or(&empty);
        let mut row = vec![
            Span::styled(format!("{:<10}", f.label.replace('_', " ")), Style::new().fg(BLUE)),
            Span::styled(format!("{:>5} rpm  ", f.rpm), Style::new().fg(if f.rpm > 0 { NEON_GREEN } else { DIM })),
        ];
        row.extend(sparkline(h, 16, true));
        lines.push(Line::from(row));
    }
    panel(Text::from(lines), "FANS", BLUE)
}

pub fn battery_panel(s: &Snapshot) -> Paragraph<'static> {
    let b = &s.battery;
    if !b.present {
        return panel(Text::styled("no battery", Style::new().fg(DIM)), "BATTERY", AMBER);
    }
    let pct = b.percent.unwrap_or(0.0);
    let pcol = if pct < 20.0 { RED } else if pct < 40.0 { AMBER } else { NEON_GREEN };
    let mut lines: Vec<Line> = Vec::new();
    let mut r = vec![Span::styled(format!("{:<9}", "charge"), Style::new().fg(DIM)),
                     Span::styled(format!("{:4.0}%  ", pct), Style::new().fg(pcol))];
    r.extend(meter(Some(pct), 100.0, 16, false));
    lines.push(Line::from(r));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "state"), Style::new().fg(DIM)),
        Span::styled(format!("{:<16}", b.status), Style::new().fg(CYAN)),
        Span::styled(format!("{:<9}", "ac"), Style::new().fg(DIM)),
        Span::styled(if b.ac_online == Some(true) { "online" } else { "offline" },
                     Style::new().fg(if b.ac_online == Some(true) { NEON_GREEN } else { DIM })),
    ]));
    if let Some(rate) = b.rate_w {
        let arrow = if rate > 0.0 { "▲" } else if rate < 0.0 { "▼" } else { "•" };
        let rcol = if rate > 0.0 { NEON_GREEN } else { AMBER };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<9}", "power"), Style::new().fg(DIM)),
            Span::styled(format!("{} {:.1} W   ", arrow, rate.abs()), Style::new().fg(rcol)),
            Span::styled(format!("{:<9}", "limit"), Style::new().fg(DIM)),
            Span::styled(b.charge_limit.map(|l| format!("{l}%")).unwrap_or_else(|| "—".into()), Style::new().fg(MAGENTA)),
        ]));
    }
    panel(Text::from(lines), "BATTERY", AMBER)
}

pub fn storage_panel(s: &Snapshot) -> Paragraph<'static> {
    let st = &s.storage;
    let nvme = st.nvme_temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "n/a".into());
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "nvme"), Style::new().fg(DIM)),
        Span::styled(format!("{:<8}", nvme), Style::new().fg(grade(st.nvme_temp_c.unwrap_or(0.0) / 80.0, true))),
        Span::styled(format!("{:<9}", "root"), Style::new().fg(DIM)),
        Span::styled(format!("{:.0}%", st.root_percent), Style::new().fg(CYAN)),
    ]));
    let mut r = vec![Span::styled(format!("{:<9}", "disk"), Style::new().fg(DIM)),
                     Span::styled(format!("{} / {}  ", human_bytes(st.root_used as f64), human_bytes(st.root_total as f64)), Style::new().fg(TEXT))];
    r.extend(meter(Some(st.root_percent), 100.0, 12, false));
    lines.push(Line::from(r));
    panel(Text::from(lines), "STORAGE", BLUE)
}

pub fn system_panel(hw: &HardwareMap, s: &Snapshot) -> Paragraph<'static> {
    let up = s.uptime_s as u64;
    let (d, rem) = (up / 86400, up % 86400);
    let (h, rem) = (rem / 3600, rem % 3600);
    let uptime = format!("{}{}h {}m", if d > 0 { format!("{d}d ") } else { String::new() }, h, rem / 60);
    let lines = vec![
        kv("host", &hw.hostname, NEON_GREEN),
        kv("model", &hw.product, TEXT),
        kv("distro", &hw.distro, TEXT),
        kv("kernel", &hw.kernel, DIM),
        kv("uptime", &uptime, CYAN),
    ];
    panel(Text::from(lines), "SYSTEM", NEON_GREEN)
}

pub fn profile_banner(s: &Snapshot) -> Paragraph<'static> {
    let prof = s.profile.clone().unwrap_or_else(|| "unknown".into());
    let (icon, color) = match prof.as_str() {
        "Quiet" => ("🌙", CYAN),
        "Balanced" => ("⚖", NEON_GREEN),
        "Performance" => ("🚀", MAGENTA),
        _ => ("•", TEXT),
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
        tiles.push((format!("{:.0}W", b.rate_w.unwrap().abs()), "SYS DRAW", AMBER));
    } else if g.present && g.power_w.is_some() {
        tiles.push((format!("{:.0}W", g.power_w.unwrap()), "GPU PWR", AMBER));
    } else {
        tiles.push((format!("{:.0}%", s.mem.percent), "RAM", CYAN));
    }

    let cell = 14usize;
    let vals: Vec<Span> = tiles.iter().map(|(v, _, c)| Span::styled(format!("{:^width$}", v, width = cell), Style::new().fg(*c).add_modifier(Modifier::BOLD))).collect();
    let labs: Vec<Span> = tiles.iter().map(|(_, l, _)| Span::styled(format!("{:^width$}", l, width = cell), Style::new().fg(DIM))).collect();
    panel(Text::from(vec![Line::from(vals), Line::from(labs)]), "SYSTEM AT A GLANCE", NEON_GREEN)
}

pub fn thermal_graph_panel(s: &Snapshot, cpu_hist: &[f64], gpu_hist: &[f64]) -> Paragraph<'static> {
    let mut lines: Vec<Line> = Vec::new();
    let ct = s.cpu.temp_c;
    lines.push(Line::from(vec![
        Span::styled("CPU °C", Style::new().fg(CYAN)),
        Span::styled(ct.map(|t| format!("   {t:.0}°C")).unwrap_or_else(|| "   —".into()), Style::new().fg(grade(ct.unwrap_or(0.0) / 95.0, true))),
        Span::styled("      scale 0–100°C", Style::new().fg(DIM)),
    ]));
    let cdata = if cpu_hist.is_empty() { vec![ct.unwrap_or(0.0)] } else { cpu_hist.to_vec() };
    lines.extend(area_chart(&cdata, 100.0, 5, None, true));
    lines.push(axis());
    if s.gpu.present {
        let gt = s.gpu.temp_c;
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("GPU °C", Style::new().fg(MAGENTA)),
            Span::styled(gt.map(|t| format!("   {t:.0}°C")).unwrap_or_else(|| "   —".into()), Style::new().fg(grade(gt.unwrap_or(0.0) / 90.0, true))),
            Span::styled("      scale 0–100°C", Style::new().fg(DIM)),
        ]));
        let gdata = if gpu_hist.is_empty() { vec![gt.unwrap_or(0.0)] } else { gpu_hist.to_vec() };
        lines.extend(area_chart(&gdata, 100.0, 5, None, true));
        lines.push(axis());
    }
    panel(Text::from(lines), "THERMAL TREND  (live)", RED)
}

pub fn fan_graph_panel(s: &Snapshot, hist: &std::collections::HashMap<String, Vec<f64>>) -> Paragraph<'static> {
    if s.fans.is_empty() {
        return panel(Text::styled("no ASUS fan channels", Style::new().fg(DIM)), "FAN SPEED", BLUE);
    }
    let mut lines: Vec<Line> = Vec::new();
    for (n, f) in s.fans.iter().enumerate() {
        if n > 0 {
            lines.push(Line::from(""));
        }
        let frac = (f.rpm as f64 / FAN_RPM_MAX).min(1.0);
        lines.push(Line::from(vec![
            Span::styled(f.label.replace('_', " ").to_uppercase(), Style::new().fg(CYAN)),
            Span::styled(format!("   {} ", f.rpm), Style::new().fg(if f.rpm > 0 { grade(frac, true) } else { DIM })),
            Span::styled("rpm", Style::new().fg(DIM)),
            Span::styled(format!("      scale 0–{} rpm", FAN_RPM_MAX as i64), Style::new().fg(DIM)),
        ]));
        let series = hist.get(&f.label).cloned().unwrap_or_else(|| vec![f.rpm as f64]);
        lines.extend(area_chart(&series, FAN_RPM_MAX, 7, None, true));
        lines.push(axis());
    }
    panel(Text::from(lines), "FAN SPEED  (live trend)", BLUE)
}

/// The temp→fan% points of a profile's CPU/GPU curves, as a readable shape.
pub fn fan_curve_panel(curves: &[crate::control::FanCurve], profile: &str, active: bool) -> Paragraph<'static> {
    let title = format!("FAN CURVE · {profile}");
    if curves.is_empty() {
        return panel(Text::styled("no fan-curve data — needs asusd", Style::new().fg(DIM)), &title, AMBER);
    }
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(profile.to_uppercase(), Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD)),
        Span::styled(
            if active { "  (active — drives the fans now)" } else { "  (edit only — not the live profile)" },
            Style::new().fg(if active { NEON_GREEN } else { DIM }),
        ),
    ]));
    for c in curves {
        let pcts = c.pwm_pcts();
        lines.push(Line::from(vec![
            Span::styled(format!("{:<4}", c.fan), Style::new().fg(CYAN)),
            Span::styled(
                if c.enabled { "curve enabled" } else { "curve disabled (firmware default)" },
                Style::new().fg(if c.enabled { NEON_GREEN } else { DIM }),
            ),
        ]));
        let temps: String = c.points.iter().map(|&(t, _)| format!("{t:>3}")).collect::<Vec<_>>().join(" ");
        lines.push(Line::from(vec![
            Span::styled("  temp ", Style::new().fg(DIM)),
            Span::styled(temps, Style::new().fg(TEXT)),
            Span::styled(" °C", Style::new().fg(DIM)),
        ]));
        let mut fl = vec![Span::styled("  fan  ", Style::new().fg(DIM))];
        for p in &pcts {
            fl.push(Span::styled(format!("{p:>3} "), Style::new().fg(grade(*p as f64 / 100.0, true))));
        }
        fl.push(Span::styled("%", Style::new().fg(DIM)));
        lines.push(Line::from(fl));
    }
    panel(Text::from(lines), &title, MAGENTA)
}

/// Detail card for the currently-selected process (Phase C).
pub fn proc_detail_panel(d: Option<&crate::telemetry::ProcDetail>) -> Paragraph<'static> {
    let Some(d) = d else {
        return panel(Text::styled("select a process to inspect", Style::new().fg(DIM)), "PROCESS DETAIL", CYAN);
    };
    let kv2 = |k: &str, v: String, vc: Color| {
        Line::from(vec![
            Span::styled(format!("{:>9} ", k), Style::new().fg(DIM)),
            Span::styled(v, Style::new().fg(vc)),
        ])
    };
    // start_time → UTC HH:MM:SS without pulling in a date crate.
    let sod = d.start_time % 86_400;
    let started = format!("{:02}:{:02}:{:02} UTC", sod / 3600, (sod % 3600) / 60, sod % 60);
    let status_col = if d.status == "run" || d.status == "sleep" || d.status == "running" || d.status == "sleeping" {
        TEXT
    } else {
        AMBER
    };
    let lines = vec![
        kv2("pid", d.pid.to_string(), NEON_GREEN),
        kv2("name", d.name.clone(), CYAN),
        kv2("status", d.status.clone(), status_col),
        kv2("user", d.user.clone(), TEXT),
        kv2("ppid", d.ppid.map(|p| p.to_string()).unwrap_or_else(|| "—".into()), TEXT),
        kv2("cpu", format!("{:.1}%", d.cpu), grade((d.cpu.min(100.0)) / 100.0, true)),
        kv2("memory", format!("{:.0} MB", d.mem_mb), CYAN),
        kv2("started", started, TEXT),
        kv2("cmd", d.cmd.chars().take(120).collect::<String>(), DIM),
    ];
    panel(Text::from(lines), "PROCESS DETAIL", CYAN)
}

pub fn gpu_proc_panel(s: &Snapshot) -> Paragraph<'static> {
    if !s.gpu.present {
        return panel(Text::styled("no GPU", Style::new().fg(DIM)), "GPU PROCESSES", MAGENTA);
    }
    if s.gpu_procs.is_empty() {
        return panel(Text::styled("no GPU compute processes", Style::new().fg(DIM)), "GPU PROCESSES", MAGENTA);
    }
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(format!("{:<7}", "pid"), Style::new().fg(DIM)),
        Span::styled(format!("{:<28}", "process"), Style::new().fg(DIM)),
        Span::styled(format!("{:>8}", "vram"), Style::new().fg(DIM)),
    ])];
    for p in &s.gpu_procs {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<7}", p.pid), Style::new().fg(DIM)),
            Span::styled(format!("{:<28}", p.name.chars().take(27).collect::<String>()), Style::new().fg(TEXT)),
            Span::styled(format!("{:>6.0}M", p.mem_mb), Style::new().fg(MAGENTA)),
        ]));
    }
    panel(Text::from(lines), "GPU PROCESSES", MAGENTA)
}

pub fn net_table(s: &Snapshot) -> Paragraph<'static> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(format!("{:<14}", "iface"), Style::new().fg(DIM)),
        Span::styled(format!("{:<6}", "state"), Style::new().fg(DIM)),
        Span::styled(format!("{:<16}", "IPv4"), Style::new().fg(DIM)),
        Span::styled(format!("{:>11}", "▼ down"), Style::new().fg(DIM)),
        Span::styled(format!("{:>11}", "▲ up"), Style::new().fg(DIM)),
        Span::styled(format!("{:>9}", "RX"), Style::new().fg(DIM)),
        Span::styled(format!("{:>9}", "TX"), Style::new().fg(DIM)),
        Span::styled(format!("{:>9}", "err/drop"), Style::new().fg(DIM)),
    ]));
    let shown: Vec<_> = s.net.iter().filter(|i| !i.is_virtual || i.up_bps + i.down_bps > 0.0).take(10).collect();
    for i in shown {
        let errs = i.errin + i.errout + i.dropin + i.dropout;
        lines.push(Line::from(vec![
            Span::styled(format!("{:<14}", format!("{}{}", i.name, if i.is_virtual { " *" } else { "" })),
                         Style::new().fg(if i.is_virtual { DIM } else { CYAN })),
            Span::styled(format!("{:<6}", if i.is_up { "UP" } else { "down" }),
                         Style::new().fg(if i.is_up { NEON_GREEN } else { DIM })),
            Span::styled(format!("{:<16}", if i.ipv4.is_empty() { "—" } else { &i.ipv4 }), Style::new().fg(TEXT)),
            Span::styled(format!("{:>11}", fmt_rate(i.down_bps).trim()), Style::new().fg(NEON_GREEN)),
            Span::styled(format!("{:>11}", fmt_rate(i.up_bps).trim()), Style::new().fg(AMBER)),
            Span::styled(format!("{:>9}", human_bytes(i.rx_total as f64)), Style::new().fg(DIM)),
            Span::styled(format!("{:>9}", human_bytes(i.tx_total as f64)), Style::new().fg(DIM)),
            Span::styled(format!("{:>9}", errs), Style::new().fg(if errs > 0 { RED } else { DIM })),
        ]));
    }
    panel(Text::from(lines), "INTERFACES  (* = virtual)", CYAN)
}

pub fn bandwidth_graph_panel(down_hist: &[f64], up_hist: &[f64], cur_down: f64, cur_up: f64) -> Paragraph<'static> {
    let dmax = down_hist.iter().cloned().fold(0.0_f64, f64::max).max(1024.0);
    let umax = up_hist.iter().cloned().fold(0.0_f64, f64::max).max(1024.0);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("▼ DOWN  ", Style::new().fg(NEON_GREEN)),
        Span::styled(fmt_rate(cur_down).trim().to_string(), Style::new().fg(NEON_GREEN)),
        Span::styled(format!("      peak {}", fmt_rate(dmax).trim()), Style::new().fg(DIM)),
    ]));
    let d = if down_hist.is_empty() { vec![0.0] } else { down_hist.to_vec() };
    lines.extend(area_chart(&d, dmax, 5, Some(NEON_GREEN), true));
    lines.push(axis());
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("▲ UP    ", Style::new().fg(AMBER)),
        Span::styled(fmt_rate(cur_up).trim().to_string(), Style::new().fg(AMBER)),
        Span::styled(format!("      peak {}", fmt_rate(umax).trim()), Style::new().fg(DIM)),
    ]));
    let u = if up_hist.is_empty() { vec![0.0] } else { up_hist.to_vec() };
    lines.extend(area_chart(&u, umax, 5, Some(AMBER), true));
    lines.push(axis());
    panel(Text::from(lines), "BANDWIDTH  (live trend)", CYAN)
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
        assert_eq!(grade(0.5, true), NEON_GREEN);
        assert_eq!(grade(0.7, true), AMBER);
        assert_eq!(grade(0.9, true), RED);
        assert_eq!(grade(0.99, false), CYAN);
    }

    #[test]
    fn area_chart_dims() {
        let rows = area_chart(&[1.0, 2.0, 3.0], 3.0, 5, None, true);
        assert_eq!(rows.len(), 5);
    }
}
