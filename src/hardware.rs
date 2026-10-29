use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::device;
use crate::nvml;
use crate::protocol::*;

const KBD_LED: &str = "/sys/class/leds/asus::kbd_backlight";
const FW_ATTRS: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";
const WMI: &str = "/sys/devices/platform/asus-nb-wmi";
const BAT: &str = "/sys/class/power_supply/BAT0";
const AC: &str = "/sys/class/power_supply/ADP0/online";
const CPU_BOOST: &str = "/sys/devices/system/cpu/cpufreq/boost";

pub type HwResult<T> = Result<T, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub operating_mode: String,
    #[serde(default)]
    pub gpu_mode: String,
    #[serde(default)]
    pub manual_enabled: bool,
    #[serde(default)]
    pub manual_ac: ManualProfile,
    #[serde(default)]
    pub manual_battery: ManualProfile,
    /// Limite di ricarica persistente (0 = non impostato).
    #[serde(default)]
    pub charge_limit: u8,
    #[serde(default = "default_nv_temp")]
    pub nv_temp_target: u8,
    #[serde(default = "default_nv_boost")]
    pub nv_dynamic_boost: u8,
    #[serde(default)]
    pub gpu_base_offset: i32,
    #[serde(default)]
    pub gpu_memory_offset: i32,
    /// Valori PPT personalizzati (None = mai impostati).
    #[serde(default)]
    pub ppt_spl: Option<u8>,
    #[serde(default)]
    pub ppt_sppt: Option<u8>,
    #[serde(default)]
    pub ppt_fppt: Option<u8>,
    /// Curve ventole della pagina Ventole.
    #[serde(default)]
    pub fan_cpu: Option<Vec<CurvePoint>>,
    #[serde(default)]
    pub fan_gpu: Option<Vec<CurvePoint>>,
    #[serde(default)]
    pub fan_cpu_enabled: Option<bool>,
    #[serde(default)]
    pub fan_gpu_enabled: Option<bool>,
    /// Altre impostazioni salvate dalla UI.
    #[serde(default)]
    pub kbd_brightness: Option<u8>,
    #[serde(default)]
    pub display_brightness: Option<u32>,
    #[serde(default)]
    pub cpu_boost: Option<bool>,
    #[serde(default)]
    pub panel_overdrive: Option<bool>,
    #[serde(default)]
    pub boot_sound: Option<bool>,
}

fn default_nv_temp() -> u8 {
    75
}

fn default_nv_boost() -> u8 {
    5
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            operating_mode: String::new(),
            gpu_mode: String::new(),
            manual_enabled: false,
            manual_ac: ManualProfile::default(),
            manual_battery: ManualProfile::default(),
            charge_limit: 0,
            nv_temp_target: default_nv_temp(),
            nv_dynamic_boost: default_nv_boost(),
            gpu_base_offset: 0,
            gpu_memory_offset: 0,
            ppt_spl: None,
            ppt_sppt: None,
            ppt_fppt: None,
            fan_cpu: None,
            fan_gpu: None,
            fan_cpu_enabled: None,
            fan_gpu_enabled: None,
            kbd_brightness: None,
            display_brightness: None,
            cpu_boost: None,
            panel_overdrive: None,
            boot_sound: None,
        }
    }
}

fn err(path: &str, e: std::io::Error) -> String {
    format!("{path}: {e}")
}

fn read_str(path: impl AsRef<Path>) -> HwResult<String> {
    let p = path.as_ref();
    fs::read_to_string(p)
        .map(|s| s.trim().to_string())
        .map_err(|e| err(&p.display().to_string(), e))
}

fn read_opt(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path.as_ref())
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_num<T: std::str::FromStr>(path: impl AsRef<Path>) -> Option<T> {
    read_opt(path)?.parse().ok()
}

fn write_str(path: impl AsRef<Path>, value: &str) -> HwResult<()> {
    let p = path.as_ref();
    let mut f = OpenOptions::new()
        .write(true)
        .open(p)
        .map_err(|e| err(&p.display().to_string(), e))?;
    f.write_all(value.as_bytes())
        .map_err(|e| err(&p.display().to_string(), e))
}

fn hwmon_by_name(name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let path = entry.path();
        if read_opt(path.join("name")).as_deref() == Some(name) {
            return Some(path);
        }
    }
    None
}

fn fw_attr(name: &str) -> Option<String> {
    read_opt(format!("{FW_ATTRS}/{name}/current_value"))
}

fn fw_attr_available(name: &str) -> bool {
    Path::new(&format!("{FW_ATTRS}/{name}/current_value")).exists()
        && fw_attr(name).is_some()
}

fn set_fw_attr(name: &str, value: &str) -> HwResult<()> {
    write_str(format!("{FW_ATTRS}/{name}/current_value"), value)
}

fn ac_online() -> bool {
    read_opt(AC).as_deref() == Some("1")
}

fn source_name() -> &'static str {
    if ac_online() { "ac" } else { "battery" }
}

/// Cerca l'hidraw del controller RGB della tastiera (0b05:19b6).
fn find_hidraw() -> Option<PathBuf> {
    for entry in fs::read_dir("/sys/class/hidraw").ok()?.flatten() {
        let name = entry.file_name();
        let uevent = entry.path().join("device/uevent");
        let Ok(text) = fs::read_to_string(&uevent) else { continue };
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("HID_ID=") {
                let parts: Vec<&str> = rest.split(':').collect();
                if parts.len() == 3 {
                    let vid = u32::from_str_radix(parts[1], 16).unwrap_or(0);
                    let pid = u32::from_str_radix(parts[2], 16).unwrap_or(0);
                    if vid == 0x0B05 && pid == 0x19B6 {
                        return Some(PathBuf::from("/dev").join(name));
                    }
                }
            }
        }
    }
    None
}

fn write_padded(f: &mut File, data: &[u8]) -> HwResult<()> {
    // Il controller si aspetta report da 64 byte: i pacchetti Aura sono 17 byte.
    let mut buf = [0u8; 64];
    let len = data.len().min(64);
    buf[..len].copy_from_slice(&data[..len]);
    f.write_all(&buf).map_err(|e| format!("scrittura hidraw: {e}"))
}

/// Effetto Aura globale. La struttura del pacchetto è quella usata da asusctl:
/// [0x5d, 0xb3, zone, mode, r, g, b, speed, direction, 0, r2, g2, b2, ...]
fn apply_aura(cfg: &AuraState) -> HwResult<()> {
    let path = find_hidraw().ok_or_else(|| {
        "controller RGB non trovato (hidraw 0b05:19b6 assente)".to_string()
    })?;
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| err(&path.display().to_string(), e))?;

    let mut effect = [0u8; 17];
    effect[0] = 0x5d;
    effect[1] = 0xb3;
    effect[2] = 0; // globale
    effect[3] = cfg.mode;
    effect[4..7].copy_from_slice(&cfg.colour1);
    effect[7] = speed_byte(cfg.speed);
    effect[8] = cfg.direction;
    effect[10..13].copy_from_slice(&cfg.colour2);

    write_padded(&mut f, &effect)?;
    std::thread::sleep(Duration::from_millis(10));
    write_padded(&mut f, &[0x5d, 0xb5])?; // set
    std::thread::sleep(Duration::from_millis(10));
    write_padded(&mut f, &[0x5d, 0xb4])?; // apply
    Ok(())
}

fn speed_byte(speed: u8) -> u8 {
    match speed {
        0 => 0xe1,
        2 => 0xf5,
        _ => 0xeb,
    }
}

fn save_aura(cfg: &AuraState) {
    if let Some(dir) = Path::new(AURA_STATE_PATH).parent() {
        let _ = fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(cfg) {
        Ok(json) => {
            if let Err(e) = fs::write(AURA_STATE_PATH, json) {
                eprintln!("impossibile salvare lo stato Aura: {e}");
            }
        }
        Err(e) => eprintln!("serializzazione Aura fallita: {e}"),
    }
}

fn load_aura() -> AuraState {
    fs::read_to_string(AURA_STATE_PATH)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &DaemonConfig) -> HwResult<()> {
    if let Some(dir) = Path::new(CONFIG_PATH).parent() {
        fs::create_dir_all(dir).map_err(|e| err(&dir.display().to_string(), e))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = format!("{CONFIG_PATH}.tmp");
    fs::write(&tmp, json).map_err(|e| err(&tmp, e))?;
    fs::rename(&tmp, CONFIG_PATH).map_err(|e| err(CONFIG_PATH, e))
}

fn mode_from_profile(profile: &str) -> String {
    match profile.trim().to_lowercase().as_str() {
        "quiet" => "silent",
        "performance" => "turbo",
        _ => "windows",
    }
    .to_string()
}

/// Profilo ACPI da scrivere per la modalità operativa salvata.
fn profile_for_mode(mode: &str) -> Option<&'static str> {
    if mode.is_empty() {
        return None;
    }
    Some(match mode {
        "silent" => "quiet",
        "turbo" => "performance",
        "manual" => "performance",
        _ => "balanced",
    })
}

fn effective_mode(cfg: &DaemonConfig, profile: &ProfileState) -> String {
    if cfg.manual_enabled {
        return "manual".to_string();
    }
    match profile.current.trim().to_lowercase().as_str() {
        "quiet" => "silent".to_string(),
        "performance" => "turbo".to_string(),
        _ => {
            if cfg.operating_mode == "performance" {
                "performance".to_string()
            } else {
                "windows".to_string()
            }
        }
    }
}

/// Alla prima esecuzione adotta i valori già presenti nell'hardware, così
/// anche le impostazioni esistenti diventano persistenti e vengono riapplicate.
fn adopt_current_settings(cfg: &mut DaemonConfig) -> bool {
    let mut changed = false;
    if cfg.kbd_brightness.is_none() {
        if let Some(v) = read_num::<u8>(format!("{KBD_LED}/brightness")) {
            cfg.kbd_brightness = Some(v);
            changed = true;
        }
    }
    if cfg.display_brightness.is_none() {
        if let Some(bl) = backlight_state() {
            cfg.display_brightness = Some(bl.brightness);
            changed = true;
        }
    }
    if cfg.cpu_boost.is_none() {
        if let Some(v) = cpu_boost() {
            cfg.cpu_boost = Some(v);
            changed = true;
        }
    }
    if cfg.panel_overdrive.is_none() {
        if let Some(v) = fw_attr("panel_overdrive").or_else(|| read_opt(format!("{WMI}/panel_od"))) {
            cfg.panel_overdrive = Some(v == "1");
            changed = true;
        }
    }
    if cfg.boot_sound.is_none() {
        if let Some(v) = fw_attr("boot_sound").or_else(|| read_opt(format!("{WMI}/boot_sound"))) {
            cfg.boot_sound = Some(v == "1");
            changed = true;
        }
    }
    changed
}

fn load_config() -> DaemonConfig {    let mut cfg: DaemonConfig = fs::read_to_string(CONFIG_PATH)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if cfg.operating_mode.is_empty() {
        cfg.operating_mode = mode_from_profile(&profile_state().current);
    }
    if cfg.manual_ac.cpu.len() != 8 {
        cfg.manual_ac = ManualProfile::default();
    }
    if cfg.manual_battery.cpu.len() != 8 {
        cfg.manual_battery = ManualProfile::default();
    }
    cfg
}

fn os_name() -> String {
    fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("PRETTY_NAME="))
                .map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".to_string())
}

fn system_info(cpu_boost: Option<bool>) -> SystemInfo {
    let uptime = read_str("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let load1 = read_str("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f32>().ok())
        .unwrap_or(0.0);

    SystemInfo {
        model: read_opt("/sys/devices/virtual/dmi/id/product_name").unwrap_or_default(),
        board: read_opt("/sys/devices/virtual/dmi/id/board_name").unwrap_or_default(),
        kernel: read_opt("/proc/sys/kernel/osrelease").unwrap_or_default(),
        os: os_name(),
        hostname: read_opt("/proc/sys/kernel/hostname").unwrap_or_default(),
        uptime_secs: uptime,
        load1,
        cpu_governor: read_opt("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
            .unwrap_or_default(),
        cpu_boost,
        daemon_version: DAEMON_VERSION.to_string(),
        socket: SOCKET_PATH.to_string(),
    }
}

fn cpu_times() -> Option<(u64, u64)> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().next()?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if fields.len() < 4 {
        return None;
    }
    let total: u64 = fields.iter().sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

fn cpu_freq_mhz() -> f32 {
    let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") else { return 0.0 };
    let mut sum = 0f32;
    let mut count = 0f32;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(num) = name.strip_prefix("cpu") else { continue };
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Some(khz) = read_num::<u64>(entry.path().join("cpufreq/scaling_cur_freq")) {
            sum += khz as f32 / 1000.0;
            count += 1.0;
        }
    }
    if count > 0.0 {
        return sum / count;
    }
    // fallback: cpu MHz da /proc/cpuinfo
    if let Ok(text) = fs::read_to_string("/proc/cpuinfo") {
        let mut sum = 0f32;
        let mut n = 0f32;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("cpu MHz") {
                if let Some(v) = rest.split(':').nth(1).and_then(|v| v.trim().parse::<f32>().ok()) {
                    sum += v;
                    n += 1.0;
                }
            }
        }
        if n > 0.0 {
            return sum / n;
        }
    }
    0.0
}

fn memory_state() -> MemoryState {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else { return MemoryState::default() };
    let mut total_kb = 0u64;
    let mut avail_kb = 0u64;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    MemoryState {
        total_mb: total_kb / 1024,
        used_mb: total_kb.saturating_sub(avail_kb) / 1024,
        available_mb: avail_kb / 1024,
    }
}

fn storage_state() -> StorageState {
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(b"/\0".as_ptr() as *const libc::c_char, &mut stat) != 0 {
            return StorageState::default();
        }
        let frsize = stat.f_frsize as u64;
        let total = stat.f_blocks as u64 * frsize;
        let free = stat.f_bavail as u64 * frsize;
        let used = total.saturating_sub(free);
        StorageState {
            total_gb: total as f32 / 1e9,
            used_gb: used as f32 / 1e9,
            free_gb: free as f32 / 1e9,
        }
    }
}

fn keyboard_state(has_rgb: bool) -> KeyboardState {
    KeyboardState {
        brightness: read_num(format!("{KBD_LED}/brightness")).unwrap_or(0),
        max_brightness: read_num(format!("{KBD_LED}/max_brightness")).unwrap_or(3),
        has_rgb,
    }
}

fn fan_states() -> Vec<FanState> {
    let Some(base) = hwmon_by_name("asus") else { return Vec::new() };
    let curve = hwmon_by_name("asus_custom_fan_curve");
    let mut out = Vec::new();

    for fan in 1u8..=2 {
        let rpm = read_num::<u32>(base.join(format!("fan{fan}_input"))).unwrap_or(0);
        let raw_label = read_opt(base.join(format!("fan{fan}_label"))).unwrap_or_default();
        let label = if raw_label.to_lowercase().contains("cpu") {
            "CPU".to_string()
        } else if raw_label.to_lowercase().contains("gpu") {
            "GPU".to_string()
        } else if fan == 1 {
            "CPU".to_string()
        } else {
            "GPU".to_string()
        };

        let mut custom = false;
        let mut points = Vec::new();
        if let Some(c) = curve.as_ref() {
            custom = read_num::<u8>(c.join(format!("pwm{fan}_enable"))) == Some(1);
            for i in 1..=8u8 {
                let temp = read_num::<u8>(c.join(format!("pwm{fan}_auto_point{i}_temp")));
                let pwm = read_num::<u8>(c.join(format!("pwm{fan}_auto_point{i}_pwm")));
                if let (Some(temp), Some(pwm)) = (temp, pwm) {
                    points.push(CurvePoint { temp, pwm });
                }
            }
        }
        if points.len() != 8 {
            points = defaults_for(fan);
        }

        out.push(FanState { fan, label, rpm, custom, points });
    }
    out
}

fn defaults_for(fan: u8) -> Vec<CurvePoint> {
    let base: [(u8, u8); 8] = if fan == 1 {
        [(40, 0), (50, 30), (60, 50), (70, 80), (78, 120), (84, 160), (90, 200), (96, 255)]
    } else {
        [(40, 0), (50, 40), (60, 60), (70, 90), (78, 130), (84, 170), (90, 210), (96, 255)]
    };
    base.iter().map(|&(temp, pwm)| CurvePoint { temp, pwm }).collect()
}

fn profile_state() -> ProfileState {
    let current = read_opt("/sys/firmware/acpi/platform_profile").unwrap_or_default();
    let choices = read_opt("/sys/firmware/acpi/platform_profile_choices")
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    ProfileState { current, choices }
}

fn battery_state() -> BatteryState {
    let power_now_w = read_num::<i64>(format!("{BAT}/power_now"))
        .map(|uw| uw as f32 / 1_000_000.0)
        .or_else(|| {
            let v = read_num::<i64>(format!("{BAT}/voltage_now"))? as f32 / 1_000_000.0;
            let i = read_num::<i64>(format!("{BAT}/current_now"))? as f32 / 1_000_000.0;
            Some((v * i).abs())
        })
        .unwrap_or(0.0);

    BatteryState {
        charge_limit: read_num(format!("{BAT}/charge_control_end_threshold")).unwrap_or(100),
        capacity: read_num(format!("{BAT}/capacity")).unwrap_or(0),
        status: read_opt(format!("{BAT}/status")).unwrap_or_default(),
        power_now_w,
        ac_online: ac_online(),
    }
}

fn ppt_state() -> PptState {
    let spl = read_num::<u8>(format!("{WMI}/ppt_pl1_spl")).unwrap_or(5);
    let sppt = read_num::<u8>(format!("{WMI}/ppt_pl2_sppt")).unwrap_or(5);
    let fppt = read_num::<u8>(format!("{WMI}/ppt_fppt")).unwrap_or(5);
    PptState { spl, sppt, fppt, auto: spl == 5 && sppt == 5 && fppt == 5 }
}

fn gpu_state(cfg: &DaemonConfig) -> GpuState {
    let panel_overdrive = fw_attr("panel_overdrive")
        .or_else(|| read_opt(format!("{WMI}/panel_od")))
        .map(|v| v == "1")
        .unwrap_or(false);
    let boot_sound = fw_attr("boot_sound")
        .or_else(|| read_opt(format!("{WMI}/boot_sound")))
        .map(|v| v == "1")
        .unwrap_or(false);
    let dgpu_disabled = fw_attr("dgpu_disable").as_deref() == Some("1");
    let mux_mode = fw_attr("gpu_mux_mode").and_then(|v| v.parse().ok()).unwrap_or(0);
    let offsets = nvml::get_offsets();
    let (base_clock_offset, memory_clock_offset, offsets_supported) = match offsets {
        Some((base, mem)) => (base, mem, true),
        None => (0, 0, false),
    };

    let derived = if dgpu_disabled {
        // Eco: dGPU disattivata; il pannello è gestito dalla iGPU (Optimus).
        "eco"
    } else if mux_mode == 0 {
        // 0 = solo dGPU (Discrete/Ultimate), 1 = Optimus/Hybrid.
        "ultimate"
    } else {
        "standard"
    };
    let mode = if cfg.gpu_mode == "optimized" && !dgpu_disabled && mux_mode == 0 {
        "optimized".to_string()
    } else {
        derived.to_string()
    };

    GpuState {
        mode,
        dgpu_disabled,
        mux_mode,
        panel_overdrive,
        boot_sound,
        charge_mode: fw_attr("charge_mode").and_then(|v| v.parse().ok()).unwrap_or(1),
        pending_reboot: read_opt(format!("{FW_ATTRS}/pending_reboot"))
            .map(|v| v == "1")
            .unwrap_or(false),
        dgpu_available: fw_attr_available("dgpu_disable"),
        mux_available: fw_attr_available("gpu_mux_mode"),
        nv_temp_target: read_num(format!("{WMI}/nv_temp_target")).unwrap_or(75),
        nv_dynamic_boost: read_num(format!("{WMI}/nv_dynamic_boost")).unwrap_or(5),
        base_clock_offset,
        memory_clock_offset,
        offsets_supported,
    }
}

fn backlight_path() -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for entry in fs::read_dir("/sys/class/backlight").ok()?.flatten() {
        let p = entry.path();
        if entry.file_name().to_string_lossy().contains("nvidia") {
            return Some(p);
        }
        if best.is_none() {
            best = Some(p);
        }
    }
    best
}

fn backlight_state() -> Option<BacklightState> {
    let p = backlight_path()?;
    Some(BacklightState {
        brightness: read_num(p.join("brightness")).unwrap_or(0),
        max_brightness: read_num(p.join("max_brightness")).unwrap_or(100),
    })
}

fn temp_label(module: &str, index: u32, explicit: Option<String>) -> String {
    match module {
        "k10temp" => {
            if index == 1 { "CPU Tctl".into() } else { format!("CPU CCD{index}") }
        }
        "amdgpu" => match index {
            1 => "iGPU edge".into(),
            2 => "iGPU junction".into(),
            3 => "iGPU mem".into(),
            _ => format!("iGPU {index}"),
        },
        "nvme" => "NVMe".into(),
        "acpitz_0" | "acpitz" => "ACPI".into(),
        "spd5118" => "RAM".into(),
        "BAT0" => "Batteria".into(),
        "mt7921_phy0" => "Wi-Fi".into(),
        m if m.starts_with("r8169") => "LAN".into(),
        other => explicit
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| other.to_string()),
    }
}

fn temps() -> Vec<TempReading> {
    let mut out: Vec<TempReading> = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else { return out };

    for entry in entries.flatten() {
        let dir = entry.path();
        let module = read_opt(dir.join("name")).unwrap_or_default();
        let Ok(files) = fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().to_string();
            let Some(stem) = fname.strip_suffix("_input") else { continue };
            if !stem.starts_with("temp") {
                continue;
            }
            let Ok(index) = stem.trim_start_matches("temp").parse::<u32>() else { continue };
            let Some(milli) = read_num::<i64>(f.path()) else { continue };
            let celsius = milli as f32 / 1000.0;
            if !(0.0..150.0).contains(&celsius) {
                continue;
            }
            let explicit = read_opt(dir.join(format!("{stem}_label")));
            let mut label = temp_label(&module, index, explicit);
            if out.iter().any(|t| t.label == label) {
                let mut suffix = 2;
                loop {
                    let candidate = format!("{label} {suffix}");
                    if !out.iter().any(|t| t.label == candidate) {
                        label = candidate;
                        break;
                    }
                    suffix += 1;
                }
            }
            out.push(TempReading { label, celsius });
        }
    }

    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

fn nvidia_info() -> Option<NvidiaInfo> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,temperature.gpu,utilization.gpu,power.draw,clocks.sm,clocks.mem,memory.used,memory.total,power.limit,power.max_limit",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next()?.trim();
    let parts: Vec<&str> = first.split(',').map(str::trim).collect();
    if parts.len() < 10 {
        return None;
    }
    let num = |s: &str| s.replace("N/A", "0").parse::<f32>().unwrap_or(0.0);
    Some(NvidiaInfo {
        name: parts[0].to_string(),
        temp_c: num(parts[1]),
        util_pct: num(parts[2]),
        power_w: num(parts[3]),
        clock_mhz: num(parts[4]),
        mem_clock_mhz: num(parts[5]),
        mem_used_mb: num(parts[6]),
        mem_total_mb: num(parts[7]),
        power_limit_w: num(parts[8]),
        max_power_limit_w: num(parts[9]),
    })
}

fn cpu_boost() -> Option<bool> {
    read_num::<u8>(CPU_BOOST).map(|v| v == 1)
}

fn clamp_range(value: u8, range: ValueRange) -> u8 {
    let min = range.min.clamp(0, 255) as u8;
    let max = range.max.clamp(0, 255) as u8;
    value.clamp(min, max)
}

/// 5 è il valore "auto/default" del firmware; gli altri vengono limitati al
/// range specifico del modello.
fn norm_ppt(value: u8, range: ValueRange) -> u8 {
    if value == 5 {
        5
    } else {
        clamp_range(value, range)
    }
}

fn validate_curve(points: &[CurvePoint]) -> Result<(), String> {
    if points.len() != 8 {
        return Err("la curva deve avere esattamente 8 punti".to_string());
    }
    let mut prev_temp = 0u8;
    let mut prev_pwm = 0u8;
    for (i, p) in points.iter().enumerate() {
        if p.temp > 120 {
            return Err(format!("punto {}: temperatura troppo alta ({}°C)", i + 1, p.temp));
        }
        if p.temp < prev_temp {
            return Err(format!("punto {}: le temperature devono essere crescenti", i + 1));
        }
        if p.pwm < prev_pwm {
            return Err(format!("punto {}: i valori PWM devono essere crescenti", i + 1));
        }
        prev_temp = p.temp;
        prev_pwm = p.pwm;
    }
    Ok(())
}

pub struct Hardware {
    pub aura: AuraState,
    pub config: DaemonConfig,
    pub limits: DeviceLimits,
    hidraw: Option<PathBuf>,
    aura_saved: bool,
    cpu_prev: Option<(u64, u64)>,
    last_state_at: Instant,
}

impl Hardware {
    pub fn new() -> Self {
        Self::build(true)
    }

    /// Costruisce l'oggetto senza riapplicare le impostazioni.
    /// Usato dai retry di avvio, che riapplicano solo ciò che dipende dal driver.
    pub fn new_without_restore() -> Self {
        Self::build(false)
    }

    fn build(restore: bool) -> Self {
        let aura_saved = Path::new(AURA_STATE_PATH).exists();
        let mut config = load_config();
        // Adotta il limite di ricarica già presente nell'EC alla prima esecuzione,
        // così l'impostazione esistente diventa persistente.
        if !(20..=100).contains(&config.charge_limit) {
            if let Some(v) = read_num::<u8>(format!("{BAT}/charge_control_end_threshold")) {
                if (20..=100).contains(&v) {
                    config.charge_limit = v;
                    let _ = save_config(&config);
                }
            }
        }
        let mut hw = Self {
            aura: load_aura(),
            config,
            limits: device::detect(),
            hidraw: find_hidraw(),
            aura_saved,
            cpu_prev: cpu_times(),
            last_state_at: Instant::now(),
        };
        if adopt_current_settings(&mut hw.config) {
            let _ = save_config(&hw.config);
        }
        if restore {
            hw.restore_persisted();
        }
        hw
    }

    /// Riapplica tutte le impostazioni salvate su disco.
    ///
    /// Viene chiamato all'avvio del demone e può essere richiamato più volte:
    /// al boot driver e dispositivi (NVIDIA, hidraw, WMI) possono non essere
    /// ancora pronti.
    pub fn restore_persisted(&mut self) {
        let errors = self.reapply_all();
        for e in errors {
            eprintln!("ripristino impostazioni: {e}");
        }
    }

    /// Riapplica solo le impostazioni che dipendono dal driver NVIDIA.
    /// Usato dai retry di avvio senza toccare luminosità o altri valori che
    /// l'utente potrebbe aver appena modificato.
    pub fn retry_gpu_settings(&self) {
        let mut errors = Vec::new();
        if let Err(e) = self.write_nv_limits(self.config.nv_temp_target, self.config.nv_dynamic_boost) {
            errors.push(format!("limiti NVIDIA: {e}"));
        }
        if self.config.gpu_base_offset != 0 || self.config.gpu_memory_offset != 0 {
            if let Err(e) = nvml::set_offsets(self.config.gpu_base_offset, self.config.gpu_memory_offset) {
                errors.push(format!("offset GPU: {e}"));
            }
        }
        for e in errors {
            eprintln!("retry impostazioni GPU: {e}");
        }
    }

    fn reapply_all(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if let Some(profile) = profile_for_mode(&self.config.operating_mode) {
            if let Err(e) = self.write_profile(profile) {
                errors.push(format!("profilo: {e}"));
            }
        }

        if (20..=100).contains(&self.config.charge_limit) {
            if let Err(e) = write_str(
                format!("{BAT}/charge_control_end_threshold"),
                &self.config.charge_limit.to_string(),
            ) {
                errors.push(format!("limite di ricarica: {e}"));
            }
        }

        if let Some(spl) = self.config.ppt_spl {
            if let Err(e) = self.write_ppt(
                spl,
                self.config.ppt_sppt.unwrap_or(spl),
                self.config.ppt_fppt.unwrap_or(spl),
            ) {
                errors.push(format!("PPT: {e}"));
            }
        }

        let fans = [
            (1u8, self.config.fan_cpu.as_deref(), self.config.fan_cpu_enabled),
            (2, self.config.fan_gpu.as_deref(), self.config.fan_gpu_enabled),
        ];
        for (fan, points, enabled) in fans {
            if enabled != Some(true) {
                continue;
            }
            let Some(points) = points else { continue };
            if let Err(e) = self.write_fan_curve(fan, points, true) {
                errors.push(format!("curva ventola {fan}: {e}"));
            }
        }

        if let Err(e) = self.write_nv_limits(self.config.nv_temp_target, self.config.nv_dynamic_boost) {
            errors.push(format!("limiti NVIDIA: {e}"));
        }
        if self.config.gpu_base_offset != 0 || self.config.gpu_memory_offset != 0 {
            if let Err(e) = nvml::set_offsets(self.config.gpu_base_offset, self.config.gpu_memory_offset) {
                errors.push(format!("offset GPU: {e}"));
            }
        }

        if let Some(v) = self.config.kbd_brightness {
            if let Err(e) = self.write_kbd_brightness(v) {
                errors.push(format!("luminosità tastiera: {e}"));
            }
        }
        if let Some(v) = self.config.display_brightness {
            if let Err(e) = self.write_display_brightness(v) {
                errors.push(format!("luminosità schermo: {e}"));
            }
        }
        if let Some(v) = self.config.cpu_boost {
            if let Err(e) = write_str(CPU_BOOST, if v { "1" } else { "0" }) {
                errors.push(format!("CPU boost: {e}"));
            }
        }
        if let Some(v) = self.config.panel_overdrive {
            if let Err(e) = self.write_panel_overdrive(v) {
                errors.push(format!("panel overdrive: {e}"));
            }
        }
        if let Some(v) = self.config.boot_sound {
            if let Err(e) = self.write_boot_sound(v) {
                errors.push(format!("boot sound: {e}"));
            }
        }

        errors
    }

    pub fn reload_config(&mut self) {
        self.config = load_config();
        self.limits = device::detect();
    }

    pub fn has_saved_aura(&self) -> bool {
        self.aura_saved
    }

    pub fn reapply_saved_aura(&self) -> HwResult<()> {
        if !self.aura_saved {
            return Ok(());
        }
        apply_aura(&self.aura)
    }

    pub fn has_rgb(&self) -> bool {
        self.hidraw.is_some() || find_hidraw().is_some()
    }

    pub fn state(&mut self) -> State {
        let profile = profile_state();
        let battery = battery_state();
        let gpu = gpu_state(&self.config);
        let operating_mode = effective_mode(&self.config, &profile);

        let now = cpu_times();
        let usage_pct = match (self.cpu_prev, now) {
            (Some((pt, pi)), Some((ct, ci))) if ct > pt => {
                let dt = (ct - pt) as f32;
                let di = ct.saturating_sub(ci).saturating_sub(pt.saturating_sub(pi));
                // idle delta = (ct - ci) - (pt - pi); usage = (dt - idle_delta) / dt
                let idle_delta = di as f32;
                ((dt - idle_delta) / dt * 100.0).clamp(0.0, 100.0)
            }
            _ => 0.0,
        };
        self.cpu_prev = now;
        self.last_state_at = Instant::now();

        let cpu = CpuState { freq_mhz: cpu_freq_mhz(), usage_pct };

        State {
            system: system_info(cpu_boost()),
            cpu,
            memory: memory_state(),
            storage: storage_state(),
            keyboard: keyboard_state(self.has_rgb()),
            aura: self.aura.clone(),
            fans: fan_states(),
            profile,
            battery,
            gpu,
            ppt: ppt_state(),
            manual: ManualState {
                enabled: self.config.manual_enabled,
                source: source_name().to_string(),
                ac: self.config.manual_ac.clone(),
                battery: self.config.manual_battery.clone(),
            },
            operating_mode,
            limits: self.limits.clone(),
            backlight: backlight_state(),
            temps: temps(),
            nvidia: nvidia_info(),
        }
    }

    fn write_kbd_brightness(&self, value: u8) -> HwResult<()> {
        let max: u8 = read_num(format!("{KBD_LED}/max_brightness")).unwrap_or(3);
        write_str(format!("{KBD_LED}/brightness"), &value.min(max).to_string())
    }

    pub fn set_kbd_brightness(&mut self, value: u8) -> HwResult<()> {
        let max: u8 = read_num(format!("{KBD_LED}/max_brightness")).unwrap_or(3);
        let value = value.min(max);
        self.write_kbd_brightness(value)?;
        self.config.kbd_brightness = Some(value);
        save_config(&self.config)
    }

    pub fn set_aura(&mut self, mode: u8, colour1: [u8; 3], colour2: [u8; 3], speed: u8, direction: u8) -> HwResult<()> {
        if mode > 12 || mode == 9 {
            return Err(format!("modalità Aura non valida: {mode}"));
        }
        if speed > 2 {
            return Err(format!("velocità Aura non valida: {speed}"));
        }
        if direction > 3 {
            return Err(format!("direzione Aura non valida: {direction}"));
        }
        let cfg = AuraState { mode, colour1, colour2, speed, direction };
        apply_aura(&cfg)?;
        self.aura = cfg;
        save_aura(&self.aura);
        Ok(())
    }

    fn write_display_brightness(&self, value: u32) -> HwResult<()> {
        let bl = backlight_state().ok_or_else(|| "backlight non trovato".to_string())?;
        let p = backlight_path().ok_or_else(|| "backlight non trovato".to_string())?;
        write_str(p.join("brightness"), &value.min(bl.max_brightness).to_string())
    }

    pub fn set_display_brightness(&mut self, value: u32) -> HwResult<()> {
        let bl = backlight_state().ok_or_else(|| "backlight non trovato".to_string())?;
        let value = value.min(bl.max_brightness);
        self.write_display_brightness(value)?;
        self.config.display_brightness = Some(value);
        save_config(&self.config)
    }

    fn write_fan_curve(&self, fan: u8, points: &[CurvePoint], enabled: bool) -> HwResult<()> {
        if !(1..=2).contains(&fan) {
            return Err(format!("ventola non valida: {fan}"));
        }
        let curve = hwmon_by_name("asus_custom_fan_curve")
            .ok_or_else(|| "driver asus_custom_fan_curve non disponibile".to_string())?;

        // In modalità automatica non tocchiamo i punti: la curva BIOS può non
        // essere monotona e non va sovrascritta.
        if !enabled {
            return write_str(curve.join(format!("pwm{fan}_enable")), "2");
        }

        validate_curve(points)?;

        for (i, p) in points.iter().enumerate() {
            write_str(curve.join(format!("pwm{fan}_auto_point{}_pwm", i + 1)), &p.pwm.to_string())?;
        }
        for (i, p) in points.iter().enumerate() {
            write_str(curve.join(format!("pwm{fan}_auto_point{}_temp", i + 1)), &p.temp.to_string())?;
        }
        write_str(curve.join(format!("pwm{fan}_enable")), "1")
    }

    pub fn set_fan_curve(&mut self, fan: u8, points: &[CurvePoint], enabled: bool) -> HwResult<()> {
        self.write_fan_curve(fan, points, enabled)?;
        match fan {
            1 => {
                self.config.fan_cpu = Some(points.to_vec());
                self.config.fan_cpu_enabled = Some(enabled);
            }
            2 => {
                self.config.fan_gpu = Some(points.to_vec());
                self.config.fan_gpu_enabled = Some(enabled);
            }
            _ => unreachable!(),
        }
        save_config(&self.config)
    }

    fn write_profile(&self, profile: &str) -> HwResult<()> {
        write_str("/sys/firmware/acpi/platform_profile", profile)
    }

    fn disable_manual_fans(&mut self) -> HwResult<()> {
        let mut errors = Vec::new();
        for fan in 1..=2 {
            if let Err(e) = self.write_fan_curve(fan, &[], false) {
                errors.push(e);
            }
        }
        if let Err(e) = self.write_ppt(5, 5, 5) {
            errors.push(format!("PPT: {e}"));
        }
        if let Err(e) = self.write_nv_limits(75, 5) {
            errors.push(format!("NVIDIA: {e}"));
        }
        // Anche lo stato salvato torna ai valori automatici del firmware.
        self.config.ppt_spl = Some(5);
        self.config.ppt_sppt = Some(5);
        self.config.ppt_fppt = Some(5);
        self.config.fan_cpu_enabled = Some(false);
        self.config.fan_gpu_enabled = Some(false);
        let _ = save_config(&self.config);
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }

    pub fn set_profile(&mut self, profile: &str) -> HwResult<()> {
        let state = profile_state();
        if !state.choices.iter().any(|c| c == profile) {
            return Err(format!("profilo non valido: {profile}"));
        }
        self.write_profile(profile)?;
        if self.config.manual_enabled {
            self.config.manual_enabled = false;
            let _ = self.disable_manual_fans();
        }
        self.config.operating_mode = mode_from_profile(profile);
        save_config(&self.config)?;
        Ok(())
    }

    pub fn set_operating_mode(&mut self, mode: &str) -> HwResult<()> {
        match mode {
            "windows" | "performance" | "silent" | "turbo" => {
                let profile = profile_for_mode(mode).unwrap();
                if self.config.manual_enabled {
                    self.config.manual_enabled = false;
                    self.disable_manual_fans()?;
                }
                self.write_profile(profile)?;
                self.config.operating_mode = mode.to_string();
                save_config(&self.config)
            }
            "manual" => {
                self.config.manual_enabled = true;
                self.config.operating_mode = "manual".to_string();
                self.write_profile("performance")?;
                save_config(&self.config)?;
                self.apply_manual_for_current_source()
            }
            other => Err(format!("modalità operativa non valida: {other}")),
        }
    }

    pub fn set_gpu_mode(&mut self, mode: &str) -> HwResult<()> {
        let (mux, dgpu) = match mode {
            // gpu_mux_mode: 0 = solo dGPU (Ultimate), 1 = Optimus/Hybrid.
            "ultimate" => (Some("0"), Some("0")),
            "standard" => (Some("1"), Some("0")),
            "eco" => (Some("1"), Some("1")),
            "optimized" => (Some("1"), Some("0")),
            other => return Err(format!("modalità GPU non valida: {other}")),
        };
        let mut errors = Vec::new();
        if let Some(v) = mux {
            if let Err(e) = set_fw_attr("gpu_mux_mode", v) {
                errors.push(e);
            }
        }
        if let Some(v) = dgpu {
            if let Err(e) = set_fw_attr("dgpu_disable", v) {
                errors.push(e);
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        self.config.gpu_mode = mode.to_string();
        save_config(&self.config)
    }

    pub fn set_ppt(&mut self, spl: u8, sppt: u8, fppt: u8) -> HwResult<()> {
        let spl = norm_ppt(spl, self.limits.ppt_spl);
        let sppt = norm_ppt(sppt, self.limits.ppt_sppt);
        let fppt = norm_ppt(fppt, self.limits.ppt_fppt);
        self.write_ppt(spl, sppt, fppt)?;
        self.config.ppt_spl = Some(spl);
        self.config.ppt_sppt = Some(sppt);
        self.config.ppt_fppt = Some(fppt);
        save_config(&self.config)
    }

    fn write_ppt(&self, spl: u8, sppt: u8, fppt: u8) -> HwResult<()> {
        let spl = norm_ppt(spl, self.limits.ppt_spl);
        let sppt = norm_ppt(sppt, self.limits.ppt_sppt);
        let fppt = norm_ppt(fppt, self.limits.ppt_fppt);
        write_str(format!("{WMI}/ppt_pl1_spl"), &spl.to_string())?;
        write_str(format!("{WMI}/ppt_pl2_sppt"), &sppt.to_string())?;
        if self.limits.fppt_supported && Path::new(&format!("{WMI}/ppt_fppt")).exists() {
            write_str(format!("{WMI}/ppt_fppt"), &fppt.to_string())?;
        }
        Ok(())
    }

    fn write_nv_limits(&self, temp_target: u8, dynamic_boost: u8) -> HwResult<()> {
        let temp = clamp_range(temp_target, self.limits.gpu_temp);
        let boost = clamp_range(dynamic_boost, self.limits.gpu_boost);
        write_str(format!("{WMI}/nv_temp_target"), &temp.to_string())?;
        write_str(format!("{WMI}/nv_dynamic_boost"), &boost.to_string())
    }

    pub fn set_nv_limits(&mut self, temp_target: u8, dynamic_boost: u8) -> HwResult<()> {
        let temp = clamp_range(temp_target, self.limits.gpu_temp);
        let boost = clamp_range(dynamic_boost, self.limits.gpu_boost);
        self.write_nv_limits(temp, boost)?;
        self.config.nv_temp_target = temp;
        self.config.nv_dynamic_boost = boost;
        save_config(&self.config)
    }

    pub fn set_gpu_offsets(&mut self, base_clock: i32, memory_clock: i32) -> HwResult<()> {
        let base = base_clock.clamp(self.limits.gpu_base.min, self.limits.gpu_base.max);
        let mem = memory_clock.clamp(self.limits.gpu_mem.min, self.limits.gpu_mem.max);
        nvml::set_offsets(base, mem)?;
        self.config.gpu_base_offset = base;
        self.config.gpu_memory_offset = mem;
        save_config(&self.config)
    }

    pub fn save_manual_profile(
        &mut self,
        source: &str,
        cpu: Vec<CurvePoint>,
        gpu: Vec<CurvePoint>,
        nv_temp_target: u8,
        nv_dynamic_boost: u8,
        spl: u8,
        sppt: u8,
        fppt: u8,
    ) -> HwResult<()> {
        validate_curve(&cpu)?;
        validate_curve(&gpu)?;
        let profile = ManualProfile {
            cpu,
            gpu,
            nv_temp_target: clamp_range(nv_temp_target, self.limits.gpu_temp),
            nv_dynamic_boost: clamp_range(nv_dynamic_boost, self.limits.gpu_boost),
            spl: norm_ppt(spl, self.limits.ppt_spl),
            sppt: norm_ppt(sppt, self.limits.ppt_sppt),
            fppt: norm_ppt(fppt, self.limits.ppt_fppt),
        };
        match source {
            "ac" => self.config.manual_ac = profile.clone(),
            "battery" => self.config.manual_battery = profile.clone(),
            other => return Err(format!("sorgente non valida: {other}")),
        }
        save_config(&self.config)?;
        if self.config.manual_enabled && source == source_name() {
            self.apply_manual_profile(&profile)?;
        }
        Ok(())
    }

    pub fn set_manual_enabled(&mut self, enabled: bool) -> HwResult<()> {
        if enabled {
            self.config.manual_enabled = true;
            self.config.operating_mode = "manual".to_string();
            self.write_profile("performance")?;
            save_config(&self.config)?;
            self.apply_manual_for_current_source()
        } else {
            self.config.manual_enabled = false;
            self.config.operating_mode = mode_from_profile(&profile_state().current);
            let _ = self.disable_manual_fans();
            save_config(&self.config)
        }
    }

    pub fn apply_manual_for_current_source(&self) -> HwResult<()> {
        let profile = if ac_online() {
            &self.config.manual_ac
        } else {
            &self.config.manual_battery
        };
        self.apply_manual_profile(profile)
    }

    fn apply_manual_profile(&self, profile: &ManualProfile) -> HwResult<()> {
        let mut errors = Vec::new();
        if let Err(e) = self.write_fan_curve(1, &profile.cpu, true) {
            errors.push(format!("CPU fan: {e}"));
        }
        if let Err(e) = self.write_fan_curve(2, &profile.gpu, true) {
            errors.push(format!("GPU fan: {e}"));
        }
        if let Err(e) = self.write_ppt(profile.spl, profile.sppt, profile.fppt) {
            errors.push(format!("PPT: {e}"));
        }
        if let Err(e) = self.write_nv_limits(profile.nv_temp_target, profile.nv_dynamic_boost) {
            errors.push(format!("NVIDIA: {e}"));
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }

    pub fn set_charge_limit(&mut self, value: u8) -> HwResult<()> {
        let v = value.clamp(20, 100);
        write_str(format!("{BAT}/charge_control_end_threshold"), &v.to_string())?;
        self.config.charge_limit = v;
        save_config(&self.config)
    }

    fn write_panel_overdrive(&self, value: bool) -> HwResult<()> {
        let v = if value { "1" } else { "0" };
        if Path::new(&format!("{FW_ATTRS}/panel_overdrive/current_value")).exists() {
            set_fw_attr("panel_overdrive", v)
        } else {
            write_str(format!("{WMI}/panel_od"), v)
        }
    }

    pub fn set_panel_overdrive(&mut self, value: bool) -> HwResult<()> {
        self.write_panel_overdrive(value)?;
        self.config.panel_overdrive = Some(value);
        save_config(&self.config)
    }

    fn write_boot_sound(&self, value: bool) -> HwResult<()> {
        let v = if value { "1" } else { "0" };
        if Path::new(&format!("{FW_ATTRS}/boot_sound/current_value")).exists() {
            set_fw_attr("boot_sound", v)
        } else {
            write_str(format!("{WMI}/boot_sound"), v)
        }
    }

    pub fn set_boot_sound(&mut self, value: bool) -> HwResult<()> {
        self.write_boot_sound(value)?;
        self.config.boot_sound = Some(value);
        save_config(&self.config)
    }

    pub fn set_dgpu_disabled(&self, value: bool) -> HwResult<()> {
        set_fw_attr("dgpu_disable", if value { "1" } else { "0" })
    }

    pub fn set_gpu_mux(&self, value: u8) -> HwResult<()> {
        if value > 1 {
            return Err(format!("modalità mux non valida: {value}"));
        }
        set_fw_attr("gpu_mux_mode", &value.to_string())
    }

    pub fn set_cpu_boost(&mut self, value: bool) -> HwResult<()> {
        write_str(CPU_BOOST, if value { "1" } else { "0" })?;
        self.config.cpu_boost = Some(value);
        save_config(&self.config)
    }
}
