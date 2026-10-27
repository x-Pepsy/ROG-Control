use serde::{Deserialize, Serialize};

/// Percorso del socket unix usato dalla GUI per parlare col demone.
pub const SOCKET_PATH: &str = "/run/rog-control/control.sock";
/// Percorso del file in cui il demone salva l'ultimo effetto Aura applicato.
/// L'effetto RGB non è leggibile dal controller, quindi va ricordato lato software.
pub const AURA_STATE_PATH: &str = "/var/lib/rog-control/aura.json";
/// Configurazione del demone: modalità operativa, modalità GPU, profili manuali.
pub const CONFIG_PATH: &str = "/var/lib/rog-control/config.json";
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Legge tutto lo stato hardware.
    State,
    SetKbdBrightness { value: u8 },
    /// Applica un effetto Aura globale (la tastiera non supporta le zone).
    SetAura {
        mode: u8,
        colour1: [u8; 3],
        colour2: [u8; 3],
        speed: u8,
        direction: u8,
    },
    SetDisplayBrightness { value: u32 },
    SetFanCurve { fan: u8, points: Vec<CurvePoint>, enabled: bool },
    SetProfile { profile: String },
    /// Modalità operativa stile Armoury Crate: windows, silent, performance, turbo, manual.
    SetOperatingMode { mode: String },
    /// Modalità GPU: ultimate, standard, eco, optimized.
    SetGpuMode { mode: String },
    /// Limiti CPU in watt (5 = auto firmware).
    SetPpt { spl: u8, sppt: u8 },
    /// Limiti GPU NVIDIA.
    SetNvLimits { temp_target: u8, dynamic_boost: u8 },
    /// Salva il profilo manuale per la sorgente indicata ("ac" o "battery").
    SaveManualProfile {
        source: String,
        cpu: Vec<CurvePoint>,
        gpu: Vec<CurvePoint>,
        nv_temp_target: u8,
        nv_dynamic_boost: u8,
        spl: u8,
        sppt: u8,
    },
    SetManualEnabled { enabled: bool },
    SetChargeLimit { value: u8 },
    SetPanelOverdrive { value: bool },
    SetBootSound { value: bool },
    SetDgpuDisabled { value: bool },
    SetGpuMux { value: u8 },
    SetCpuBoost { value: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    pub error: Option<String>,
    pub state: Option<State>,
}

impl Response {
    pub fn ok(id: u64, state: State) -> Self {
        Self { id, ok: true, error: None, state: Some(state) }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self { id, ok: false, error: Some(error.into()), state: None }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub system: SystemInfo,
    pub cpu: CpuState,
    pub memory: MemoryState,
    pub storage: StorageState,
    pub keyboard: KeyboardState,
    pub aura: AuraState,
    pub fans: Vec<FanState>,
    pub profile: ProfileState,
    pub battery: BatteryState,
    pub gpu: GpuState,
    pub ppt: PptState,
    pub manual: ManualState,
    pub operating_mode: String,
    pub backlight: Option<BacklightState>,
    pub temps: Vec<TempReading>,
    pub nvidia: Option<NvidiaInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemInfo {
    pub model: String,
    pub board: String,
    pub kernel: String,
    pub os: String,
    pub hostname: String,
    pub uptime_secs: u64,
    pub load1: f32,
    pub cpu_governor: String,
    pub cpu_boost: Option<bool>,
    pub daemon_version: String,
    pub socket: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuState {
    pub freq_mhz: f32,
    pub usage_pct: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryState {
    pub total_mb: u64,
    pub used_mb: u64,
    pub available_mb: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageState {
    pub total_gb: f32,
    pub used_gb: f32,
    pub free_gb: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyboardState {
    pub brightness: u8,
    pub max_brightness: u8,
    pub has_rgb: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuraState {
    pub mode: u8,
    pub colour1: [u8; 3],
    pub colour2: [u8; 3],
    pub speed: u8,
    pub direction: u8,
}

impl Default for AuraState {
    fn default() -> Self {
        Self {
            mode: 0,
            colour1: [255, 40, 60],
            colour2: [0, 0, 255],
            speed: 1,
            direction: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FanState {
    pub fan: u8,
    pub label: String,
    pub rpm: u32,
    pub custom: bool,
    pub points: Vec<CurvePoint>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CurvePoint {
    pub temp: u8,
    pub pwm: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileState {
    pub current: String,
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatteryState {
    pub charge_limit: u8,
    pub capacity: u8,
    pub status: String,
    pub power_now_w: f32,
    pub ac_online: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuState {
    pub mode: String,
    pub dgpu_disabled: bool,
    pub mux_mode: u8,
    pub panel_overdrive: bool,
    pub boot_sound: bool,
    pub charge_mode: u8,
    pub pending_reboot: bool,
    pub dgpu_available: bool,
    pub mux_available: bool,
    pub nv_temp_target: u8,
    pub nv_dynamic_boost: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PptState {
    pub spl: u8,
    pub sppt: u8,
    pub auto: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManualState {
    pub enabled: bool,
    pub source: String,
    pub ac: ManualProfile,
    pub battery: ManualProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualProfile {
    pub cpu: Vec<CurvePoint>,
    pub gpu: Vec<CurvePoint>,
    pub nv_temp_target: u8,
    pub nv_dynamic_boost: u8,
    pub spl: u8,
    pub sppt: u8,
}

impl Default for ManualProfile {
    fn default() -> Self {
        Self {
            cpu: default_curve(1),
            gpu: default_curve(2),
            nv_temp_target: 75,
            nv_dynamic_boost: 5,
            spl: 5,
            sppt: 5,
        }
    }
}

fn default_curve(fan: u8) -> Vec<CurvePoint> {
    let base: [(u8, u8); 8] = if fan == 1 {
        [(40, 0), (50, 30), (60, 50), (70, 80), (78, 120), (84, 160), (90, 200), (96, 255)]
    } else {
        [(40, 0), (50, 40), (60, 60), (70, 90), (78, 130), (84, 170), (90, 210), (96, 255)]
    };
    base.iter().map(|&(temp, pwm)| CurvePoint { temp, pwm }).collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BacklightState {
    pub brightness: u32,
    pub max_brightness: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TempReading {
    pub label: String,
    pub celsius: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NvidiaInfo {
    pub name: String,
    pub temp_c: f32,
    pub util_pct: f32,
    pub power_w: f32,
    pub clock_mhz: f32,
    pub mem_clock_mhz: f32,
    pub mem_used_mb: f32,
    pub mem_total_mb: f32,
    pub power_limit_w: f32,
    pub max_power_limit_w: f32,
}
