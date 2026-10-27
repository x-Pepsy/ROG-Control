use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::protocol::*;

const KBD_LED: &str = "/sys/class/leds/asus::kbd_backlight";
const FW_ATTRS: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";
const WMI: &str = "/sys/devices/platform/asus-nb-wmi";
const BAT: &str = "/sys/class/power_supply/BAT0";
const CPU_BOOST: &str = "/sys/devices/system/cpu/cpufreq/boost";

pub type HwResult<T> = Result<T, String>;

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
        let label = read_opt(base.join(format!("fan{fan}_label")))
            .unwrap_or_else(|| if fan == 1 { "CPU".into() } else { "GPU".into() });
        let label = if label.to_lowercase().contains("cpu") {
            "CPU".to_string()
        } else if label.to_lowercase().contains("gpu") {
            "GPU".to_string()
        } else if label.is_empty() {
            if fan == 1 { "CPU".to_string() } else { "GPU".to_string() }
        } else {
            label
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
    // Curva prudente di riferimento, solo per il primo avvio/letture fallite.
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
    }
}

fn gpu_state() -> GpuState {
    let panel_overdrive = fw_attr("panel_overdrive")
        .or_else(|| read_opt(format!("{WMI}/panel_od")))
        .map(|v| v == "1")
        .unwrap_or(false);
    let boot_sound = fw_attr("boot_sound")
        .or_else(|| read_opt(format!("{WMI}/boot_sound")))
        .map(|v| v == "1")
        .unwrap_or(false);

    GpuState {
        dgpu_disabled: fw_attr("dgpu_disable").as_deref() == Some("1"),
        mux_mode: fw_attr("gpu_mux_mode").and_then(|v| v.parse().ok()).unwrap_or(0),
        panel_overdrive,
        boot_sound,
        charge_mode: fw_attr("charge_mode").and_then(|v| v.parse().ok()).unwrap_or(1),
        pending_reboot: read_opt(format!("{FW_ATTRS}/pending_reboot"))
            .map(|v| v == "1")
            .unwrap_or(false),
        dgpu_available: fw_attr_available("dgpu_disable"),
        mux_available: fw_attr_available("gpu_mux_mode"),
    }
}

fn backlight_state() -> Option<BacklightState> {
    let mut best: Option<PathBuf> = None;
    for entry in fs::read_dir("/sys/class/backlight").ok()?.flatten() {
        let p = entry.path();
        if entry.file_name().to_string_lossy().contains("nvidia") {
            best = Some(p);
            break;
        }
        if best.is_none() {
            best = Some(p);
        }
    }
    let p = best?;
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
            "--query-gpu=name,temperature.gpu,utilization.gpu,power.draw,clocks.sm",
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
    if parts.len() < 5 {
        return None;
    }
    let num = |s: &str| s.replace("N/A", "0").parse::<f32>().unwrap_or(0.0);
    Some(NvidiaInfo {
        name: parts[0].to_string(),
        temp_c: num(parts[1]),
        util_pct: num(parts[2]),
        power_w: num(parts[3]),
        clock_mhz: num(parts[4]),
    })
}

fn cpu_boost() -> Option<bool> {
    read_num::<u8>(CPU_BOOST).map(|v| v == 1)
}

pub struct Hardware {
    pub aura: AuraState,
    hidraw: Option<PathBuf>,
    aura_saved: bool,
}

impl Hardware {
    pub fn new() -> Self {
        let aura_saved = Path::new(AURA_STATE_PATH).exists();
        Self { aura: load_aura(), hidraw: find_hidraw(), aura_saved }
    }

    /// Ripristina l'ultimo effetto Aura salvato (usato all'avvio del demone).
    pub fn reapply_saved_aura(&self) -> HwResult<()> {
        if !self.aura_saved {
            return Ok(());
        }
        apply_aura(&self.aura)
    }

    pub fn has_saved_aura(&self) -> bool {
        self.aura_saved
    }

    pub fn has_rgb(&self) -> bool {
        self.hidraw.is_some() || find_hidraw().is_some()
    }

    pub fn state(&self) -> State {
        let has_rgb = self.has_rgb();
        State {
            system: system_info(cpu_boost()),
            keyboard: keyboard_state(has_rgb),
            aura: self.aura.clone(),
            fans: fan_states(),
            profile: profile_state(),
            battery: battery_state(),
            gpu: gpu_state(),
            backlight: backlight_state(),
            temps: temps(),
            nvidia: nvidia_info(),
        }
    }

    pub fn set_kbd_brightness(&self, value: u8) -> HwResult<()> {
        let max: u8 = read_num(format!("{KBD_LED}/max_brightness")).unwrap_or(3);
        let v = value.min(max);
        write_str(format!("{KBD_LED}/brightness"), &v.to_string())
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

    pub fn set_display_brightness(&self, value: u32) -> HwResult<()> {
        let bl = backlight_state().ok_or_else(|| "backlight non trovato".to_string())?;
        let v = value.min(bl.max_brightness);
        // Rilegge il percorso per non dipendere dalla struct
        let mut best: Option<PathBuf> = None;
        for entry in fs::read_dir("/sys/class/backlight")
            .map_err(|e| err("/sys/class/backlight", e))?
            .flatten()
        {
            if entry.file_name().to_string_lossy().contains("nvidia") {
                best = Some(entry.path());
                break;
            }
            if best.is_none() {
                best = Some(entry.path());
            }
        }
        let p = best.ok_or_else(|| "backlight non trovato".to_string())?;
        write_str(p.join("brightness"), &v.to_string())
    }

    pub fn set_fan_curve(&self, fan: u8, points: &[CurvePoint], enabled: bool) -> HwResult<()> {
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

        for (i, p) in points.iter().enumerate() {
            write_str(curve.join(format!("pwm{fan}_auto_point{}_pwm", i + 1)), &p.pwm.to_string())?;
        }
        for (i, p) in points.iter().enumerate() {
            write_str(curve.join(format!("pwm{fan}_auto_point{}_temp", i + 1)), &p.temp.to_string())?;
        }
        write_str(curve.join(format!("pwm{fan}_enable")), "1")
    }

    pub fn set_profile(&self, profile: &str) -> HwResult<()> {
        let state = profile_state();
        if !state.choices.iter().any(|c| c == profile) {
            return Err(format!("profilo non valido: {profile}"));
        }
        write_str("/sys/firmware/acpi/platform_profile", profile)
    }

    pub fn set_charge_limit(&self, value: u8) -> HwResult<()> {
        let v = value.clamp(20, 100);
        write_str(format!("{BAT}/charge_control_end_threshold"), &v.to_string())
    }

    pub fn set_panel_overdrive(&self, value: bool) -> HwResult<()> {
        let v = if value { "1" } else { "0" };
        if Path::new(&format!("{FW_ATTRS}/panel_overdrive/current_value")).exists() {
            set_fw_attr("panel_overdrive", v)
        } else {
            write_str(format!("{WMI}/panel_od"), v)
        }
    }

    pub fn set_boot_sound(&self, value: bool) -> HwResult<()> {
        let v = if value { "1" } else { "0" };
        if Path::new(&format!("{FW_ATTRS}/boot_sound/current_value")).exists() {
            set_fw_attr("boot_sound", v)
        } else {
            write_str(format!("{WMI}/boot_sound"), v)
        }
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

    pub fn set_cpu_boost(&self, value: bool) -> HwResult<()> {
        write_str(CPU_BOOST, if value { "1" } else { "0" })
    }
}
