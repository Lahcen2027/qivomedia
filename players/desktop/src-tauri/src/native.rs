//! What the native shell adds to the web player: device health, screenshots,
//! reboot, display power, keep-awake and autostart.
//! Windows: Win32 APIs. Linux (kiosk boxes on Debian with X11): systemd + xset.
//! The app reports which commands work on this machine (`capabilities`), and the
//! dashboard only offers those.

use std::{io::Cursor, sync::Mutex};

use base64::Engine;
use serde_json::{json, Value};

/// Keeps CPU readings between calls (CPU % needs two samples).
pub struct Sys(pub Mutex<sysinfo::System>);

impl Default for Sys {
    fn default() -> Self {
        let mut s = sysinfo::System::new();
        s.refresh_cpu_usage();
        s.refresh_memory();
        Self(Mutex::new(s))
    }
}

pub fn device_info(sys: &Sys) -> Value {
    let (cpu, mem_used, mem_total) = {
        let mut s = sys.0.lock().unwrap();
        s.refresh_cpu_usage();
        s.refresh_memory();
        (s.global_cpu_usage(), s.used_memory(), s.total_memory())
    };
    let disks = sysinfo::Disks::new_with_refreshed_list();
    // The disk holding the player's data (C: on a normal install).
    let home = crate::config::home();
    let disk = disks
        .list()
        .iter()
        .filter(|d| home.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .or_else(|| disks.list().first());
    let temps: Vec<f32> = sysinfo::Components::new_with_refreshed_list().list().iter().filter_map(|c| c.temperature()).filter(|t| *t > 0.0 && *t < 150.0).collect();
    let max_temp = temps.iter().cloned().fold(f32::NAN, f32::max);
    let monitors: Vec<Value> = xcap::Monitor::all()
        .map(|ms| {
            ms.iter()
                .map(|m| {
                    json!({
                        "name": m.name().unwrap_or_default(),
                        "width": m.width().unwrap_or_default(),
                        "height": m.height().unwrap_or_default(),
                        "hz": m.frequency().unwrap_or_default(),
                        "scale": m.scale_factor().unwrap_or(1.0),
                        "primary": m.is_primary().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "app": "qivo-player",
        "app_version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "os": sysinfo::System::long_os_version().unwrap_or_default(),
        "hostname": sysinfo::System::host_name().unwrap_or_default(),
        "cpu_percent": (f64::from(cpu) * 10.0).round() / 10.0,
        "cpu_cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "memory_used_mb": mem_used / 1_048_576,
        "memory_total_mb": mem_total / 1_048_576,
        "disk_free_gb": disk.map(|d| (d.available_space() as f64 / 1e9 * 10.0).round() / 10.0),
        "disk_total_gb": disk.map(|d| (d.total_space() as f64 / 1e9 * 10.0).round() / 10.0),
        "temperature_c": if max_temp.is_nan() { Value::Null } else { json!(max_temp.round()) },
        "uptime_s": sysinfo::System::uptime(),
        "monitors": monitors,
        "capabilities": capabilities(),
    })
}

/// Native commands this build can actually carry out.
pub fn capabilities() -> Vec<&'static str> {
    let mut c = vec!["screenshot", "restart_app"];
    if cfg!(windows) {
        c.extend(["reboot", "display_off", "display_on"]);
    }
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/run/systemd/system").exists() {
            c.push("reboot");
        }
        if x11() && x11_has_dpms() {
            c.extend(["display_off", "display_on"]);
        }
    }
    c
}

/// An X11 session with `xset` available (screen power and blanking control).
#[cfg(target_os = "linux")]
fn x11() -> bool {
    std::env::var_os("DISPLAY").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_none() && which("xset")
}

/// The X server can switch the screen's power (not true of virtual displays).
#[cfg(target_os = "linux")]
fn x11_has_dpms() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| {
        std::process::Command::new("xset")
            .arg("q")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("DPMS is"))
            .unwrap_or(false)
    })
}

#[cfg(target_os = "linux")]
fn which(cmd: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file())).unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn xset(args: &[&str]) -> Result<(), String> {
    let st = std::process::Command::new("xset").args(args).status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err(format!("xset {} failed", args.join(" "))) }
}

/// JPEG of the primary screen, at most 1920 px wide, base64-encoded.
pub fn screenshot() -> Result<String, String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let m = monitors.iter().find(|m| m.is_primary().unwrap_or(false)).or_else(|| monitors.first()).ok_or("no screen found")?;
    let img = m.capture_image().map_err(|e| e.to_string())?;
    let img = image::DynamicImage::ImageRgba8(img);
    let img = if img.width() > 1920 { img.resize(1920, 1920, image::imageops::FilterType::Triangle) } else { img };
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .encode_image(&img.to_rgb8())
        .map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(out.into_inner()))
}

pub fn reboot() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        crate::config::log("reboot requested from the dashboard");
        std::process::Command::new("shutdown")
            .args(["/r", "/t", "10", "/c", "Qivomedia: restart requested from the dashboard"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        // Allowed for the kiosk account by the polkit rule that setup-kiosk.sh installs.
        crate::config::log("reboot requested from the dashboard");
        let out = std::process::Command::new("systemctl").arg("reboot").output().map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()) }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    Err("reboot is not supported on this system".into())
}

/// Turn the screen's signal off (most monitors and many TVs go to standby) or back on.
pub fn display_power(on: bool) -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, HWND_BROADCAST, SC_MONITORPOWER, WM_SYSCOMMAND};
        if on {
            PostMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER as usize, -1isize);
            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
        } else {
            PostMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER as usize, 2);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        if !x11() || !x11_has_dpms() {
            return Err("screen power needs an X11 display with power management (DPMS)".into());
        }
        if on {
            xset(&["dpms", "force", "on"])?;
            xset(&["s", "reset"])?;
            xset(&["-dpms"]) // and never blank again by itself
        } else {
            xset(&["+dpms"])?;
            xset(&["dpms", "force", "off"])
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = on;
        Err("display power is not supported on this system".into())
    }
}

/// Never let Windows blank the screen or sleep while the player runs.
/// Must be called on a thread that lives as long as the app (the main thread).
pub fn keep_awake() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
        SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
    }
    #[cfg(target_os = "linux")]
    if x11() {
        // No screensaver, no blanking, no power saving.
        let _ = xset(&["s", "off"]);
        let _ = xset(&["s", "noblank"]);
        let _ = xset(&["-dpms"]);
    }
}

/// Start the player when this user's desktop session starts (XDG autostart).
#[cfg(target_os = "linux")]
pub fn set_autostart(on: bool) {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
        .join("autostart");
    let file = dir.join("qivomedia-player.desktop");
    if on {
        let Ok(exe) = std::env::current_exe() else { return };
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(file, format!("[Desktop Entry]\nType=Application\nName=Qivomedia Player\nExec=\"{}\"\nX-GNOME-Autostart-enabled=true\n", exe.display()));
    } else {
        let _ = std::fs::remove_file(file);
    }
}

/// Start the player when this Windows user signs in (HKCU\…\Run).
#[cfg(windows)]
pub fn set_autostart(on: bool) {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let Ok((run, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run") else { return };
    if on {
        if let Ok(exe) = std::env::current_exe() {
            let _ = run.set_value("QivomediaPlayer", &format!("\"{}\"", exe.display()));
        }
    } else {
        let _ = run.delete_value("QivomediaPlayer");
    }
}
