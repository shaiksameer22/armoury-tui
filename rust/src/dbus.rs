//! zbus proxies for the daemons we talk to.
//!
//! Verified against **asusd 6.3.1** on the reference TUF A15 via
//! `busctl introspect xyz.ljones.Asusd`. This is the modern property-based API
//! (bus `xyz.ljones.Asusd`, not the old `org.asuslinux.Daemon`):
//!
//!   /xyz/ljones        xyz.ljones.Platform   — PlatformProfile (u), ChargeControlEndThreshold (y)
//!   /xyz/ljones        xyz.ljones.FanCurves  — FanCurveData(u), SetFanCurve, SetFanCurvesEnabled, …
//!   /xyz/ljones/aura/* xyz.ljones.Aura       — Brightness (u), LedMode (u), LedModeData (…)
//!
//! Blocking zbus is used deliberately: control actions run via `spawn_blocking`
//! (like the UPower reads in telemetry), so the whole controller stays sync.

use anyhow::{anyhow, Result};
use zbus::blocking::{Connection, Proxy};

pub const ASUSD: &str = "xyz.ljones.Asusd";
pub const ROOT: &str = "/xyz/ljones";
pub const PLATFORM: &str = "xyz.ljones.Platform";
pub const FANCURVES: &str = "xyz.ljones.FanCurves";
pub const AURA: &str = "xyz.ljones.Aura";

/// asusd fan curves are fixed 8-point temp→pwm byte arrays.
pub type Curve8 = (u8, u8, u8, u8, u8, u8, u8, u8);
/// One curve as returned/accepted by asusd: (fan name, temps°C, pwm 0-255, enabled).
pub type RawCurve = (String, Curve8, Curve8, bool);

pub struct Bus {
    conn: Connection,
    aura_path: Option<String>,
}

impl Bus {
    /// Connect to the system bus and locate the per-device Aura object.
    pub fn connect() -> Result<Bus> {
        let conn = Connection::system()?;
        let aura_path = discover_aura(&conn);
        Ok(Bus { conn, aura_path })
    }

    fn proxy(&self, path: String, iface: &'static str) -> Result<Proxy<'_>> {
        Proxy::new(&self.conn, ASUSD, path, iface).map_err(Into::into)
    }

    pub fn platform(&self) -> Result<Proxy<'_>> {
        self.proxy(ROOT.to_string(), PLATFORM)
    }

    pub fn fancurves(&self) -> Result<Proxy<'_>> {
        self.proxy(ROOT.to_string(), FANCURVES)
    }

    pub fn aura(&self) -> Result<Proxy<'_>> {
        let path = self
            .aura_path
            .clone()
            .ok_or_else(|| anyhow!("no Aura device on this machine"))?;
        self.proxy(path, AURA)
    }
}

/// Walk `/xyz/ljones/aura` and return its first child object path. The leaf node
/// is device-specific (`tuf`, `rog`, a board id, …), so we discover rather than
/// hardcode it.
fn discover_aura(conn: &Connection) -> Option<String> {
    let proxy = Proxy::new(
        conn,
        ASUSD,
        "/xyz/ljones/aura",
        "org.freedesktop.DBus.Introspectable",
    )
    .ok()?;
    let xml: String = proxy.call("Introspect", &()).ok()?;
    let child = xml.split("<node name=\"").nth(1)?.split('"').next()?;
    if child.is_empty() {
        return None;
    }
    Some(format!("/xyz/ljones/aura/{child}"))
}
