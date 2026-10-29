use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, RichText, Stroke, TextStyle,
};
use serde::{Deserialize, Serialize};

use rog_control::protocol::*;

const ACCENT: Color32 = Color32::from_rgb(255, 42, 78);
const ACCENT_SOFT: Color32 = Color32::from_rgb(46, 13, 23);
const GREEN: Color32 = Color32::from_rgb(86, 211, 130);
const YELLOW: Color32 = Color32::from_rgb(240, 196, 80);
const RED: Color32 = Color32::from_rgb(255, 92, 92);
const BG: Color32 = Color32::from_rgb(8, 8, 11);
const BG_ALT: Color32 = Color32::from_rgb(11, 11, 15);
const PANEL: Color32 = Color32::from_rgb(17, 17, 22);
const PANEL_HI: Color32 = Color32::from_rgb(23, 23, 30);
const BORDER: Color32 = Color32::from_rgb(36, 36, 46);
const BORDER_HI: Color32 = Color32::from_rgb(58, 58, 72);
const TEXT: Color32 = Color32::from_rgb(237, 238, 244);
const MUTED: Color32 = Color32::from_rgb(138, 141, 155);
const TRACK: Color32 = Color32::from_rgb(31, 31, 40);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 800.0])
            .with_min_inner_size([920.0, 620.0])
            .with_title("ROG Control")
            .with_icon(egui::IconData {
                rgba: include_bytes!("../assets/icon_256.rgba").to_vec(),
                width: 256,
                height: 256,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "ROG Control",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

fn rpc(command: Command) -> Result<State, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let request = Request { id, command };

    let mut stream = UnixStream::connect(SOCKET_PATH)
        .map_err(|e| format!("demone non raggiungibile ({e})"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;

    let mut payload = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    payload.push(b'\n');
    stream.write_all(&payload).map_err(|e| e.to_string())?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| format!("lettura risposta: {e}"))?;
    let response: Response =
        serde_json::from_str(line.trim()).map_err(|e| format!("risposta non valida: {e}"))?;
    if !response.ok {
        return Err(response.error.unwrap_or_else(|| "errore sconosciuto".to_string()));
    }
    response.state.ok_or_else(|| "risposta senza stato".to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Home,
    Keyboard,
    Fans,
    Power,
    Battery,
    Gpu,
    System,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Keyboard => "Tastiera",
            Tab::Fans => "Ventole",
            Tab::Power => "Alimentazione",
            Tab::Battery => "Batteria",
            Tab::Gpu => "GPU e display",
            Tab::System => "Sistema",
        }
    }
}

#[derive(Clone)]
struct FanDraft {
    temps: [u8; 8],
    pct: [u8; 8],
}

impl FanDraft {
    fn from_points(points: &[CurvePoint]) -> Self {
        let mut d = Self { temps: [40, 50, 60, 70, 78, 84, 90, 96], pct: [0, 25, 40, 55, 70, 80, 90, 100] };
        for (i, p) in points.iter().take(8).enumerate() {
            d.temps[i] = p.temp;
            d.pct[i] = pwm_to_pct(p.pwm);
        }
        d
    }

    fn points(&self) -> Vec<CurvePoint> {
        self.temps
            .iter()
            .zip(self.pct.iter())
            .map(|(&temp, &pct)| CurvePoint { temp, pwm: pct_to_pwm(pct) })
            .collect()
    }

    fn validate(&self) -> Result<(), String> {
        for i in 1..8 {
            if self.temps[i] < self.temps[i - 1] {
                return Err(format!("punto {}: la temperatura deve essere >= del precedente", i + 1));
            }
            if self.pct[i] < self.pct[i - 1] {
                return Err(format!("punto {}: la percentuale deve essere >= della precedente", i + 1));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct SessionConfig {
    screen_saver: bool,
    touchpad_enabled: bool,
    win_key_enabled: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { screen_saver: false, touchpad_enabled: true, win_key_enabled: true }
    }
}

#[derive(Clone)]
struct DisplayInfo {
    output: String,
    mode: String,
    current: f32,
    min: f32,
    max: f32,
}

fn session_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/rog-control/session.json"))
}

fn load_session() -> SessionConfig {
    session_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store_session(cfg: &SessionConfig) {
    let Some(path) = session_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, json);
    }
}

fn run_cmd(command: &str, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(command)
        .args(args)
        .output()
        .map_err(|e| format!("{command}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn touchpad_id() -> Option<String> {
    let out = run_cmd("xinput", &["list"]).ok()?;
    for line in out.lines() {
        if line.to_lowercase().contains("touchpad") {
            if let Some(pos) = line.find("id=") {
                return line[pos + 3..]
                    .split_whitespace()
                    .next()
                    .map(str::to_string);
            }
        }
    }
    None
}

fn read_touchpad() -> Option<bool> {
    let id = touchpad_id()?;
    let out = run_cmd("xinput", &["list-props", &id]).ok()?;
    for line in out.lines() {
        if line.contains("Device Enabled") {
            let value = line.rsplit(':').next()?.trim();
            return Some(value == "1");
        }
    }
    None
}

fn set_touchpad(enabled: bool) -> Result<(), String> {
    let id = touchpad_id().ok_or_else(|| "touchpad non trovato".to_string())?;
    run_cmd(
        "xinput",
        &["set-prop", &id, "Device Enabled", if enabled { "1" } else { "0" }],
    )
    .map(|_| ())
}

fn super_keycode() -> Option<u32> {
    let out = run_cmd("xmodmap", &["-pk"]).ok()?;
    for line in out.lines() {
        if line.contains("Super_L") {
            return line.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn read_win_key() -> Option<bool> {
    let out = run_cmd("xmodmap", &["-pk"]).ok()?;
    Some(out.lines().any(|l| l.contains("Super_L")))
}

fn set_win_key(enabled: bool) -> Result<(), String> {
    let code = super_keycode().ok_or_else(|| "tasto Super non trovato".to_string())?;
    let expr = if enabled {
        format!("keycode {code} = Super_L")
    } else {
        format!("keycode {code} = NoSymbol")
    };
    run_cmd("xmodmap", &["-e", &expr]).map(|_| ())
}

fn display_info() -> Option<DisplayInfo> {
    let out = run_cmd("xrandr", &["--query"]).ok()?;
    let mut output: Option<String> = None;
    let mut mode = String::new();
    let mut rates: Vec<f32> = Vec::new();
    let mut current = 0.0f32;

    for line in out.lines() {
        if line.contains(" connected") && !line.contains("disconnected") {
            output = line.split_whitespace().next().map(str::to_string);
            rates.clear();
            current = 0.0;
            continue;
        }
        if output.is_none() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            // fine del blocco dell'output
            if !rates.is_empty() {
                break;
            }
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(mode_token) = tokens.next() else { continue };
        let mut line_is_current = false;
        for token in tokens {
            let is_current = token.contains('*');
            let cleaned = token.trim_matches(|c| c == '*' || c == '+');
            if let Ok(rate) = cleaned.parse::<f32>() {
                rates.push(rate);
                if is_current {
                    current = rate;
                    line_is_current = true;
                }
            }
        }
        if line_is_current {
            mode = mode_token.to_string();
        }
    }

    let output = output?;
    if rates.is_empty() {
        return None;
    }
    let min = rates.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = rates.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    Some(DisplayInfo { output, mode, current, min, max })
}

fn set_refresh(info: &DisplayInfo, rate: f32) -> Result<(), String> {
    run_cmd(
        "xrandr",
        &[
            "--output",
            &info.output,
            "--mode",
            &info.mode,
            "--rate",
            &format!("{rate:.2}"),
        ],
    )
    .map(|_| ())
}

#[derive(Clone)]
enum ConfirmAction {
    GpuMode(String),
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

struct App {
    state: Option<State>,
    error: Option<String>,
    tab: Tab,
    aura_mode: u8,
    aura_c1: [u8; 3],
    aura_c2: [u8; 3],
    aura_speed: u8,
    aura_dir: u8,
    kbd_brightness: u8,
    fans: [FanDraft; 2],
    charge_limit: u8,
    display_brightness: u32,
    ppt_spl: u8,
    ppt_sppt: u8,
    ppt_fppt: u8,
    nv_temp: u8,
    nv_boost: u8,
    gpu_base: i32,
    gpu_mem: i32,
    touchpad_enabled: bool,
    screen_saver: bool,
    win_key_enabled: bool,
    display: Option<DisplayInfo>,
    last_ac: Option<bool>,
    last_session: Instant,
    confirm: Option<ConfirmAction>,
    toast: Option<Toast>,
    last_poll: Instant,
    drafts_ready: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        let session = load_session();
        let mut app = Self {
            state: None,
            error: None,
            tab: Tab::Home,
            aura_mode: 0,
            aura_c1: [255, 40, 60],
            aura_c2: [0, 0, 255],
            aura_speed: 1,
            aura_dir: 0,
            kbd_brightness: 3,
            fans: [
                FanDraft::from_points(&[]),
                FanDraft::from_points(&[]),
            ],
            charge_limit: 100,
            display_brightness: 100,
            ppt_spl: 5,
            ppt_sppt: 5,
            ppt_fppt: 5,
            nv_temp: 75,
            nv_boost: 5,
            gpu_base: 0,
            gpu_mem: 0,
            touchpad_enabled: session.touchpad_enabled,
            screen_saver: session.screen_saver,
            win_key_enabled: session.win_key_enabled,
            display: display_info(),
            last_ac: None,
            last_session: Instant::now(),
            confirm: None,
            toast: None,
            last_poll: Instant::now(),
            drafts_ready: false,
        };
        app.refresh();
        app.sync_session_info();
        app
    }

    fn sync_session_info(&mut self) {
        if let Some(v) = read_touchpad() {
            self.touchpad_enabled = v;
        }
        if let Some(v) = read_win_key() {
            self.win_key_enabled = v;
        }
        self.display = display_info();
    }

    fn refresh(&mut self) {
        match rpc(Command::State) {
            Ok(state) => {
                self.state = Some(state);
                self.error = None;
                if !self.drafts_ready {
                    self.sync_drafts();
                }
            }
            Err(e) => {
                self.error = Some(e);
            }
        }
    }

    fn sync_drafts(&mut self) {
        let Some(state) = self.state.clone() else { return };
        self.aura_mode = state.aura.mode;
        self.aura_c1 = state.aura.colour1;
        self.aura_c2 = state.aura.colour2;
        self.aura_speed = state.aura.speed;
        self.aura_dir = state.aura.direction;
        self.kbd_brightness = state.keyboard.brightness;
        for i in 0..2 {
            if let Some(fan) = state.fans.get(i) {
                self.fans[i] = FanDraft::from_points(&fan.points);
            }
        }
        self.charge_limit = state.battery.charge_limit;
        self.ppt_spl = state.ppt.spl.max(state.limits.ppt_spl.min as u8);
        self.ppt_sppt = state.ppt.sppt.max(state.limits.ppt_sppt.min as u8);
        self.ppt_fppt = state.ppt.fppt.max(state.limits.ppt_fppt.min as u8);
        self.nv_temp = state.gpu.nv_temp_target;
        self.nv_boost = state.gpu.nv_dynamic_boost;
        self.gpu_base = state.gpu.base_clock_offset;
        self.gpu_mem = state.gpu.memory_clock_offset;
        if let Some(bl) = &state.backlight {
            self.display_brightness = bl.brightness;
        }
        self.drafts_ready = true;
    }

    fn apply_screen_policy(&mut self, ac: bool) {
        if !self.screen_saver {
            return;
        }
        let Some(info) = self.display.clone() else { return };
        let target = if ac { info.max } else { info.min };
        if (info.current - target).abs() > 0.5 {
            match set_refresh(&info, target) {
                Ok(()) => self.notify(format!(
                    "Schermo a {target:.0} Hz ({})",
                    if ac { "AC" } else { "batteria" }
                )),
                Err(e) => {
                    self.toast = Some(Toast { text: e, error: true, at: Instant::now() });
                }
            }
            self.display = display_info();
        }
    }

    fn send(&mut self, command: Command) {
        match rpc(command) {
            Ok(state) => {
                self.state = Some(state);
                self.error = None;
            }
            Err(e) => {
                self.toast = Some(Toast { text: e.clone(), error: true, at: Instant::now() });
                // Un comando rifiutato non significa demone offline.
                if self.state.is_none() {
                    self.error = Some(e);
                }
            }
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast { text: text.into(), error: false, at: Instant::now() });
    }

    fn cpu_temp(&self) -> Option<f32> {
        let state = self.state.as_ref()?;
        state
            .temps
            .iter()
            .find(|t| t.label.contains("CPU (Tctl)"))
            .or_else(|| state.temps.iter().find(|t| t.label.starts_with("CPU")))
            .map(|t| t.celsius)
    }

    fn gpu_temp(&self) -> Option<f32> {
        let state = self.state.as_ref()?;
        if let Some(n) = &state.nvidia {
            return Some(n.temp_c);
        }
        state
            .temps
            .iter()
            .find(|t| t.label == "iGPU")
            .or_else(|| state.temps.iter().find(|t| t.label.contains("iGPU")))
            .map(|t| t.celsius)
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("topbar")
            .exact_size(58.0)
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(18, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let (color, label) = if self.error.is_some() {
                        (RED, "DEMONE OFFLINE")
                    } else if self.state.is_some() {
                        (GREEN, "DEMONE ONLINE")
                    } else {
                        (YELLOW, "CONNESSIONE")
                    };
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, color);
                    ui.painter()
                        .circle_filled(rect.center(), 7.0, with_alpha(color, 40));
                    ui.add_space(2.0);
                    ui.label(RichText::new(label).size(9.0).color(color).strong());
                    ui.add_space(14.0);
                    if let Some(state) = &self.state {
                        ui.label(RichText::new(&state.system.model).size(12.0).color(MUTED));
                        ui.label(RichText::new("·").size(12.0).color(BORDER_HI));
                        ui.label(
                            RichText::new(format!("kernel {}", state.system.kernel))
                                .size(10.5)
                                .color(MUTED),
                        );
                    } else {
                        ui.label(RichText::new("ROG CONTROL").size(12.0).color(MUTED));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Refresh).clicked() {
                            self.refresh();
                        }
                        ui.add_space(2.0);
                        if let Some(state) = &self.state {
                            chip(
                                ui,
                                if state.battery.ac_online { "ALIMENTAZIONE" } else { "BATTERIA" },
                                if state.battery.ac_online { GREEN } else { YELLOW },
                            );
                            if let Some(t) = self.gpu_temp() {
                                chip(ui, &format!("GPU {t:.0}°C"), temp_color(t));
                            }
                            if let Some(t) = self.cpu_temp() {
                                chip(ui, &format!("CPU {t:.0}°C"), temp_color(t));
                            }
                        }
                    });
                });
            });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .exact_size(238.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(BG_ALT)
                    .inner_margin(Margin::symmetric(14, 0)),
            )
            .show(ui, |ui| {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("ROG").font(logo(52.0)).color(ACCENT));
                    ui.add_space(2.0);
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(120.0, 1.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(1), BORDER_HI);
                    ui.add_space(7.0);
                    ui.label(
                        RichText::new("C O N T R O L")
                            .size(9.0)
                            .color(MUTED),
                    );
                });
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("HARDWARE CONSOLE")
                            .size(8.0)
                            .color(Color32::from_rgb(103, 104, 118)),
                    );
                });
                ui.add_space(12.0);

                for tab in [
                    Tab::Home,
                    Tab::Keyboard,
                    Tab::Fans,
                    Tab::Power,
                    Tab::Battery,
                    Tab::Gpu,
                    Tab::System,
                ] {
                    let icon = match tab {
                        Tab::Home => Icon::Home,
                        Tab::Keyboard => Icon::Keyboard,
                        Tab::Fans => Icon::Fan,
                        Tab::Power => Icon::Bolt,
                        Tab::Battery => Icon::Battery,
                        Tab::Gpu => Icon::Gpu,
                        Tab::System => Icon::Sliders,
                    };
                    if nav_item(ui, icon, tab.label(), self.tab == tab).clicked() {
                        self.tab = tab;
                    }
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(format!("v{}  ·  ROG Control", DAEMON_VERSION))
                            .size(9.0)
                            .color(Color32::from_rgb(96, 97, 110)),
                    );
                });
            });
    }

    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(err) = self.error.clone() else { return };
        Frame::new()
            .fill(mix(PANEL, RED, 0.10))
            .stroke(Stroke::new(1.0, with_alpha(RED, 140)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    paint_icon(ui.painter(), rect, RED, Icon::Power);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Demone non raggiungibile").size(12.0).color(RED).strong());
                        ui.label(RichText::new(err).size(10.0).color(MUTED));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Riprova").clicked() {
                            self.refresh();
                        }
                        if secondary_button(ui, "Avvia demone").clicked() {
                            let _ = std::process::Command::new("pkexec")
                                .args(["systemctl", "start", "rog-control-daemon"])
                                .spawn();
                            self.notify("Richiesta di avvio inviata a polkit…");
                        }
                    });
                });
            });
        ui.add_space(12.0);
    }

    fn toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else { return };
        if toast.at.elapsed() > Duration::from_secs(5) {
            self.toast = None;
            return;
        }
        let color = if toast.error { RED } else { GREEN };
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-24.0, -24.0))
            .interactable(false)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(PANEL_HI)
                    .stroke(Stroke::new(1.0, BORDER_HI))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&toast.text).size(11.5).color(color));
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(action) = self.confirm.clone() else { return };
        let mut open = true;
        egui::Window::new("Conferma operazione")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                    paint_icon(ui.painter(), rect, YELLOW, Icon::Power);
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("MODIFICA HARDWARE")
                            .size(9.0)
                            .color(ACCENT)
                            .strong(),
                    );
                });
                ui.add_space(8.0);
                match &action {
                    ConfirmAction::GpuMode(mode) => {
                        let (title, detail) = match mode.as_str() {
                            "ultimate" => (
                                "Passare a Ultimate?",
                                "Solo dGPU, massime prestazioni video e consumi più alti. Richiede un riavvio.",
                            ),
                            "eco" => (
                                "Passare a Eco?",
                                "La dGPU viene disattivata: massima autonomia e meno calore, ma la GPU NVIDIA non sarà disponibile. Richiede un riavvio.",
                            ),
                            "optimized" => (
                                "Passare a Ottimizzato?",
                                "Hybrid/Optimus con gestione dinamica della dGPU da parte di Linux. Potrebbe richiedere un riavvio se cambia il mux.",
                            ),
                            _ => (
                                "Passare a Standard?",
                                "Hybrid/Optimus: la dGPU resta disponibile su richiesta. Richiede un riavvio se cambia il mux.",
                            ),
                        };
                        ui.label(RichText::new(title).font(logo(18.0)).color(TEXT));
                        ui.add_space(6.0);
                        ui.label(RichText::new(detail).size(11.0).color(MUTED));
                    }
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if secondary_button(ui, "Annulla").clicked() {
                        self.confirm = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Conferma").clicked() {
                            self.confirm = None;
                            match action {
                                ConfirmAction::GpuMode(mode) => {
                                    self.send(Command::SetGpuMode { mode });
                                    self.notify("Modalità GPU salvata: riavvia se richiesto");
                                }
                            }
                        }
                    });
                });
            });
        if !open {
            self.confirm = None;
        }
    }

    fn home_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Dashboard", "Panoramica del sistema, modalità operative e sensori in tempo reale");
        ui.add_space(16.0);

        // Hero: modalità attiva + temperature.
        Frame::new()
            .fill(mix(PANEL, ACCENT_SOFT, 0.45))
            .stroke(Stroke::new(1.0, with_alpha(ACCENT, 110)))
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::same(20))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("MODALITÀ ATTIVA").size(9.0).color(ACCENT).strong());
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(mode_title(&state.operating_mode))
                                .font(logo(34.0))
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new(mode_desc(&state.operating_mode))
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            chip(ui, &format!("PROFILO {}", state.profile.current.to_uppercase()), YELLOW);
                            chip(
                                ui,
                                &format!("GPU {}", gpu_mode_title(&state.gpu.mode).to_uppercase()),
                                GREEN,
                            );
                            chip(
                                ui,
                                if state.battery.ac_online { "RETE ELETTRICA" } else { "BATTERIA" },
                                if state.battery.ac_online { GREEN } else { YELLOW },
                            );
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(t) = self.gpu_temp() {
                            ring_gauge(ui, 92.0, t / 100.0, temp_color(t), &format!("{t:.0}°"), "GPU");
                        }
                        ui.add_space(8.0);
                        if let Some(t) = self.cpu_temp() {
                            ring_gauge(ui, 92.0, t / 100.0, temp_color(t), &format!("{t:.0}°"), "CPU");
                        }
                    });
                });
            });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Modalità operativa");
            });
            ui.add_space(12.0);
            let gap = 6.0;
            let width = ((ui.available_width() - gap * 4.0) / 5.0).max(120.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (key, _title, _desc) in OPERATING_MODES.iter() {
                    let icon = match *key {
                        "windows" => Icon::Grid,
                        "silent" => Icon::Moon,
                        "performance" => Icon::Speed,
                        "turbo" => Icon::Bolt,
                        _ => Icon::Sliders,
                    };
                    if mode_tile(
                        ui,
                        width,
                        76.0,
                        icon,
                        mode_title(key),
                        mode_desc_short(key),
                        state.operating_mode == *key,
                    )
                    .clicked()
                    {
                        self.send(Command::SetOperatingMode { mode: key.to_string() });
                        self.notify(format!("Modalità: {}", mode_title(key)));
                    }
                }
            });
        });

        ui.add_space(14.0);
        ui.columns(3, |cols| {
            // CPU
            card(&mut cols[0], |ui| {
                ui.set_min_height(176.0);
                section_title(ui, "CPU");
                ui.add_space(8.0);
                let temp = self.cpu_temp();
                ui.horizontal(|ui| {
                    metric(
                        ui,
                        "Frequenza",
                        &format!("{:.2}", state.cpu.freq_mhz / 1000.0),
                        "GHz",
                        TEXT,
                    );
                    ui.add_space(16.0);
                    metric(
                        ui,
                        "Temperatura",
                        &temp.map(|t| format!("{t:.0}")).unwrap_or_else(|| "—".into()),
                        "°C",
                        temp.map(temp_color).unwrap_or(MUTED),
                    );
                });
                ui.add_space(10.0);
                meter(
                    ui,
                    "Utilizzo",
                    &format!("{:.0} %", state.cpu.usage_pct),
                    state.cpu.usage_pct / 100.0,
                    ACCENT,
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!(
                        "{} · governor {}",
                        state.system.model, state.system.cpu_governor
                    ))
                    .size(9.5)
                    .color(MUTED),
                );
            });

            // GPU
            card(&mut cols[1], |ui| {
                ui.set_min_height(176.0);
                section_title(ui, "GPU");
                ui.add_space(8.0);
                if let Some(n) = &state.nvidia {
                    ui.horizontal(|ui| {
                        metric(
                            ui,
                            "Clock core",
                            &format!("{:.0}", n.clock_mhz),
                            "MHz",
                            TEXT,
                        );
                        ui.add_space(16.0);
                        metric(
                            ui,
                            "Temperatura",
                            &format!("{:.0}", n.temp_c),
                            "°C",
                            temp_color(n.temp_c),
                        );
                    });
                    ui.add_space(10.0);
                    meter(
                        ui,
                        "Utilizzo GPU",
                        &format!("{:.0} %", n.util_pct),
                        n.util_pct / 100.0,
                        GREEN,
                    );
                    meter(
                        ui,
                        "Potenza",
                        &format!("{:.0} / {:.0} W", n.power_w, n.max_power_limit_w.max(1.0)),
                        n.power_w / n.max_power_limit_w.max(1.0),
                        YELLOW,
                    );
                } else {
                    ui.label(RichText::new("MODALITÀ GRAFICA").size(9.0).color(MUTED));
                    ui.label(
                        RichText::new(gpu_mode_title(&state.gpu.mode))
                            .font(logo(22.0))
                            .color(TEXT),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        chip(
                            ui,
                            &format!(
                                "MUX {}",
                                if state.gpu.mux_mode == 0 { "ULTIMATE" } else { "HYBRID" }
                            ),
                            if state.gpu.mux_mode == 0 { ACCENT } else { GREEN },
                        );
                        chip(
                            ui,
                            if state.gpu.dgpu_disabled { "dGPU OFF" } else { "dGPU ON" },
                            if state.gpu.dgpu_disabled { YELLOW } else { GREEN },
                        );
                    });
                }
            });

            // Fans
            card(&mut cols[2], |ui| {
                ui.set_min_height(176.0);
                section_title(ui, "Ventole");
                ui.add_space(8.0);
                for fan in &state.fans {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&fan.label).size(10.0).color(MUTED));
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{}", fan.rpm))
                                        .font(logo(22.0))
                                        .color(ACCENT),
                                );
                                ui.label(RichText::new("RPM").size(9.0).color(MUTED));
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            chip(
                                ui,
                                if fan.custom { "CUSTOM" } else { "AUTO" },
                                if fan.custom { YELLOW } else { GREEN },
                            );
                        });
                    });
                    ui.add_space(4.0);
                }
                ui.label(
                    RichText::new("Pressione acustica non esposta dall'EC su Linux.")
                        .size(9.0)
                        .color(MUTED),
                );
            });
        });

        ui.add_space(12.0);
        ui.columns(3, |cols| {
            // Memory / storage
            card(&mut cols[0], |ui| {
                ui.set_min_height(158.0);
                section_title(ui, "Memoria");
                ui.add_space(10.0);
                let mem_pct = if state.memory.total_mb > 0 {
                    state.memory.used_mb as f32 / state.memory.total_mb as f32
                } else {
                    0.0
                };
                meter(
                    ui,
                    "RAM",
                    &format!(
                        "{:.1} / {:.1} GB",
                        state.memory.used_mb as f32 / 1024.0,
                        state.memory.total_mb as f32 / 1024.0
                    ),
                    mem_pct,
                    ACCENT,
                );
                ui.add_space(6.0);
                let disk_pct = if state.storage.total_gb > 0.0 {
                    state.storage.used_gb / state.storage.total_gb
                } else {
                    0.0
                };
                meter(
                    ui,
                    "Archiviazione",
                    &format!(
                        "{:.0} / {:.0} GB",
                        state.storage.used_gb, state.storage.total_gb
                    ),
                    disk_pct,
                    YELLOW,
                );
            });

            // Battery
            card(&mut cols[1], |ui| {
                ui.set_min_height(158.0);
                section_title(ui, "Batteria");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ring_gauge(
                        ui,
                        90.0,
                        state.battery.capacity as f32 / 100.0,
                        if state.battery.capacity >= 20 { GREEN } else { RED },
                        &format!("{}%", state.battery.capacity),
                        "carica",
                    );
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        metric(
                            ui,
                            "Consumo",
                            &format!("{:.1}", state.battery.power_now_w),
                            "W",
                            TEXT,
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!("Limite {}%", state.battery.charge_limit))
                                .size(10.0)
                                .color(YELLOW),
                        );
                        ui.label(RichText::new(battery_status_label(&state.battery.status)).size(10.0).color(MUTED));
                    });
                });
            });

            // System
            card(&mut cols[2], |ui| {
                ui.set_min_height(158.0);
                section_title(ui, "Sistema");
                ui.add_space(8.0);
                egui::Grid::new("home_sys")
                    .num_columns(2)
                    .spacing([12.0, 5.0])
                    .show(ui, |ui| {
                        row(ui, "Kernel", &state.system.kernel);
                        row(ui, "Distro", &state.system.os);
                        row(ui, "Uptime", &format_uptime(state.system.uptime_secs));
                        row(ui, "Load", &format!("{:.2}", state.system.load1));
                        row(ui, "GPU", gpu_mode_title(&state.gpu.mode));
                    });
            });
        });
    }

    fn keyboard_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Tastiera", "Retroilluminazione, livelli e illuminazione Aura RGB");
        ui.add_space(16.0);

        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.set_min_height(212.0);
                section_title(ui, "Retroilluminazione");
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Luminosità del LED gestita direttamente dal firmware ASUS.")
                        .size(10.5)
                        .color(MUTED),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}", self.kbd_brightness))
                            .font(logo(46.0))
                            .color(ACCENT),
                    );
                    ui.vertical(|ui| {
                        ui.label(RichText::new("LIVELLO").size(9.0).color(MUTED));
                        ui.label(
                            RichText::new(format!("su {}", state.keyboard.max_brightness))
                                .size(11.0)
                                .color(TEXT),
                        );
                    });
                });
                ui.add_space(4.0);
                let max = state.keyboard.max_brightness.max(1);
                ui.spacing_mut().slider_width = ui.available_width();
                let resp = ui.add(
                    egui::Slider::new(&mut self.kbd_brightness, 0..=max)
                        .show_value(false)
                        .trailing_fill(true),
                );
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                    self.send(Command::SetKbdBrightness { value: self.kbd_brightness });
                }
                ui.add_space(8.0);
                let mut clicked: Option<u8> = None;
                let gap = 6.0;
                let width = ((ui.available_width() - gap * (max as f32)) / (max as f32 + 1.0)).max(50.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for v in 0..=max {
                        let name = match v {
                            0 => "Spenta",
                            1 => "Bassa",
                            2 => "Media",
                            _ => "Alta",
                        };
                        let selected = self.kbd_brightness == v;
                        let response = ui.add_sized(
                            [width, 38.0],
                            egui::Button::new(
                                RichText::new(name).size(10.5).color(if selected {
                                    TEXT
                                } else {
                                    MUTED
                                }),
                            )
                            .fill(if selected { ACCENT_SOFT } else { PANEL_HI })
                            .stroke(Stroke::new(
                                if selected { 1.2 } else { 1.0 },
                                if selected { ACCENT } else { BORDER },
                            ))
                            .corner_radius(CornerRadius::same(8)),
                        );
                        if response.clicked() {
                            clicked = Some(v);
                        }
                    }
                });
                if let Some(v) = clicked {
                    self.kbd_brightness = v;
                    self.send(Command::SetKbdBrightness { value: v });
                }
            });

            card(&mut cols[1], |ui| {
                ui.set_min_height(212.0);
                section_title(ui, "Anteprima Aura");
                ui.add_space(10.0);
                let c1 = Color32::from_rgb(self.aura_c1[0], self.aura_c1[1], self.aura_c1[2]);
                let c2 = Color32::from_rgb(self.aura_c2[0], self.aura_c2[1], self.aura_c2[2]);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 104.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(10), Color32::from_rgb(8, 8, 11));
                let inner = rect.shrink(1.0);
                let slices = 48;
                for i in 0..slices {
                    let t = i as f32 / (slices - 1) as f32;
                    let col = mix(c1, c2, t);
                    let x0 = inner.left() + inner.width() * i as f32 / slices as f32;
                    let x1 = inner.left() + inner.width() * (i + 1) as f32 / slices as f32 + 0.5;
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, inner.top()),
                            egui::pos2(x1, inner.bottom()),
                        ),
                        CornerRadius::same(0),
                        col,
                    );
                }
                painter.rect_stroke(
                    rect,
                    CornerRadius::same(10),
                    Stroke::new(1.0, with_alpha(c1, 140)),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    egui::pos2(rect.left() + 14.0, rect.top() + 14.0),
                    Align2::LEFT_TOP,
                    aura_mode_name(self.aura_mode).to_uppercase(),
                    logo(17.0),
                    Color32::WHITE,
                );
                painter.text(
                    egui::pos2(rect.left() + 14.0, rect.bottom() - 16.0),
                    Align2::LEFT_BOTTOM,
                    format!(
                        "{}  ·  {}  ·  {}",
                        hex(self.aura_c1),
                        speed_name(self.aura_speed),
                        direction_name(self.aura_dir)
                    ),
                    FontId::proportional(10.0),
                    Color32::WHITE,
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    chip(ui, "EFFETTO GLOBALE", GREEN);
                    chip(
                        ui,
                        if state.keyboard.has_rgb { "CONTROLLER ONLINE" } else { "CONTROLLER ASSENTE" },
                        if state.keyboard.has_rgb { GREEN } else { RED },
                    );
                });
            });
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Effetti Aura");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("un solo effetto globale · nessun supporto zone")
                            .size(9.5)
                            .color(MUTED),
                    );
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Effetti scritti direttamente nel controller USB della tastiera. Aura Sync, AI e Aura Creator restano esclusiva di Armoury Crate.",
                )
                .size(10.5)
                .color(MUTED),
            );
            ui.add_space(12.0);

            let gap = 6.0;
            let cols = 6usize;
            let width = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32).max(80.0);
            for row in AURA_MODES.chunks(cols) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (name, value) in row.iter() {
                        let selected = self.aura_mode == *value;
                        let response = ui.add_sized(
                            [width, 40.0],
                            egui::Button::new(
                                RichText::new(*name).size(10.0).color(if selected {
                                    Color32::WHITE
                                } else {
                                    MUTED
                                }),
                            )
                            .fill(if selected { ACCENT_SOFT } else { PANEL_HI })
                            .stroke(Stroke::new(
                                if selected { 1.2 } else { 1.0 },
                                if selected { ACCENT } else { BORDER },
                            ))
                            .corner_radius(CornerRadius::same(8)),
                        );
                        if response.clicked() {
                            self.aura_mode = *value;
                        }
                    }
                });
                ui.add_space(4.0);
            }

            ui.add_space(14.0);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(1), BORDER);
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("COLORE PRIMARIO").size(8.5).color(MUTED));
                    ui.horizontal(|ui| {
                        ui.color_edit_button_srgb(&mut self.aura_c1);
                        ui.label(
                            RichText::new(hex(self.aura_c1))
                                .size(10.0)
                                .monospace()
                                .color(MUTED),
                        );
                    });
                });
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("COLORE SECONDARIO").size(8.5).color(MUTED));
                    ui.horizontal(|ui| {
                        ui.color_edit_button_srgb(&mut self.aura_c2);
                        ui.label(
                            RichText::new(hex(self.aura_c2))
                                .size(10.0)
                                .monospace()
                                .color(MUTED),
                        );
                    });
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("VELOCITÀ").size(8.5).color(MUTED));
                    egui::ComboBox::from_id_salt("aura_speed")
                        .selected_text(speed_name(self.aura_speed))
                        .width(96.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.aura_speed, 0, "Bassa");
                            ui.selectable_value(&mut self.aura_speed, 1, "Media");
                            ui.selectable_value(&mut self.aura_speed, 2, "Alta");
                        });
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("DIREZIONE").size(8.5).color(MUTED));
                    egui::ComboBox::from_id_salt("aura_dir")
                        .selected_text(direction_name(self.aura_dir))
                        .width(96.0)
                        .show_ui(ui, |ui| {
                            for (i, name) in ["Destra", "Sinistra", "Su", "Giù"].iter().enumerate() {
                                ui.selectable_value(&mut self.aura_dir, i as u8, *name);
                            }
                        });
                });
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "Applica effetto").clicked() {
                    let mode = self.aura_mode;
                    let c1 = self.aura_c1;
                    let c2 = self.aura_c2;
                    let speed = self.aura_speed;
                    let dir = self.aura_dir;
                    self.send(Command::SetAura {
                        mode,
                        colour1: c1,
                        colour2: c2,
                        speed,
                        direction: dir,
                    });
                    self.notify("Effetto Aura applicato");
                }
                if secondary_button(ui, "Spegni LED").clicked() {
                    self.kbd_brightness = 0;
                    self.send(Command::SetKbdBrightness { value: 0 });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("Le modifiche vengono applicate solo con Applica effetto.")
                            .size(9.0)
                            .color(MUTED),
                    );
                });
            });
            if state.keyboard.brightness == 0 {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Il LED è spento: riaccendilo dal cursore della luminosità.")
                        .size(10.0)
                        .color(YELLOW),
                );
            }
        });
    }

    fn fans_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Ventole", "Controllo automatico BIOS o curva personalizzata a 8 punti");
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            chip(ui, "CURVA A 8 PUNTI", ACCENT);
            chip(ui, "DRIVER ASUS_CUSTOM_FAN_CURVE", GREEN);
            ui.label(
                RichText::new("Temperature e percentuali devono essere crescenti.")
                    .size(9.5)
                    .color(MUTED),
            );
        });
        ui.add_space(14.0);

        let count = state.fans.len().min(2);
        if count == 0 {
            card(ui, |ui| {
                ui.label(RichText::new("Nessuna ventola rilevata.").size(12.0).color(MUTED));
            });
            return;
        }

        ui.columns(2, |cols| {
            for idx in 0..count {
                let fan = &state.fans[idx];
                let current = if fan.fan == 1 { self.cpu_temp() } else { self.gpu_temp() };
                card(&mut cols[idx], |ui| {
                    ui.set_min_height(430.0);
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(format!("{} FAN", fan.label.to_uppercase()))
                                    .font(logo(15.0))
                                    .color(TEXT),
                            );
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{}", fan.rpm))
                                        .font(logo(26.0))
                                        .color(ACCENT),
                                );
                                ui.label(RichText::new("RPM").size(9.0).color(MUTED));
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            chip(
                                ui,
                                if fan.custom { "CUSTOM" } else { "AUTO BIOS" },
                                if fan.custom { YELLOW } else { GREEN },
                            );
                            if let Some(t) = current {
                                chip(ui, &format!("{t:.0}°C"), temp_color(t));
                            }
                        });
                    });
                    ui.add_space(10.0);
                    fan_graph(ui, &self.fans[idx], current);
                    if self.fans[idx].validate().is_err() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "La curva BIOS letta non è monotona: correggi i valori prima di applicare.",
                            )
                            .size(9.5)
                            .color(YELLOW),
                        );
                    }
                    ui.add_space(10.0);

                    egui::Grid::new(format!("fan_points_{idx}"))
                        .num_columns(2)
                        .spacing([8.0, 7.0])
                        .show(ui, |ui| {
                            for p in 0..8 {
                                Frame::new()
                                    .fill(BG)
                                    .stroke(Stroke::new(1.0, BORDER))
                                    .corner_radius(CornerRadius::same(7))
                                    .inner_margin(Margin::same(6))
                                    .show(ui, |ui| {
                                        ui.set_width(174.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(format!("#{}", p + 1))
                                                    .size(8.5)
                                                    .color(ACCENT),
                                            );
                                            ui.add_sized(
                                                [66.0, 20.0],
                                                egui::DragValue::new(&mut self.fans[idx].temps[p])
                                                    .range(20..=110)
                                                    .speed(0.5)
                                                    .suffix(" °C"),
                                            );
                                            ui.add_sized(
                                                [56.0, 20.0],
                                                egui::DragValue::new(&mut self.fans[idx].pct[p])
                                                    .range(0..=100)
                                                    .speed(0.5)
                                                    .suffix(" %"),
                                            );
                                        });
                                    });
                                if (p + 1) % 2 == 0 {
                                    ui.end_row();
                                }
                            }
                        });

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if primary_button(ui, "Applica curva").clicked() {
                            match self.fans[idx].validate() {
                                Ok(()) => {
                                    let points = self.fans[idx].points();
                                    self.send(Command::SetFanCurve {
                                        fan: fan.fan,
                                        points,
                                        enabled: true,
                                    });
                                    self.notify(format!("Curva {} applicata", fan.label));
                                }
                                Err(e) => {
                                    self.toast =
                                        Some(Toast { text: e, error: true, at: Instant::now() });
                                }
                            }
                        }
                        if secondary_button(ui, "Auto BIOS").clicked() {
                            let points = self.fans[idx].points();
                            self.send(Command::SetFanCurve {
                                fan: fan.fan,
                                points,
                                enabled: false,
                            });
                            self.notify(format!("{} in modalità automatica", fan.label));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, Icon::Refresh).clicked() {
                                self.drafts_ready = false;
                                self.refresh();
                            }
                        });
                    });
                });
            }
        });
    }

    fn power_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Alimentazione", "Profili firmware, limiti CPU/GPU e modalità manuale");
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            chip(
                ui,
                &format!("PROFILO {} · {}", state.limits.profile, state.limits.source.to_uppercase()),
                ACCENT,
            );
            if state.limits.source == "generic" {
                chip(ui, "LIMITI GENERICI · VERIFICA IL MODELLO", YELLOW);
            }
            if state.limits.fppt_supported {
                chip(ui, "FPPT DISPONIBILE", GREEN);
            } else {
                chip(ui, "FPPT NON SUPPORTATO", YELLOW);
            }
        });
        ui.add_space(14.0);

        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.set_min_height(330.0);
                ui.horizontal(|ui| {
                    section_title(ui, "Limiti CPU");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if state.ppt.auto {
                            chip(ui, "AUTO FIRMWARE", YELLOW);
                        } else {
                            chip(ui, "PERSONALIZZATO", ACCENT);
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("SPL sostenuto · SPPT breve durata · FPPT picco. 5 W = auto.")
                        .size(9.5)
                        .color(MUTED),
                );
                ui.add_space(12.0);
                let spl = state.limits.ppt_spl;
                let sppt = state.limits.ppt_sppt;
                let fppt = state.limits.ppt_fppt;

                slider_header(ui, "SPL", &format!("{} W", self.ppt_spl), ACCENT);
                ui.spacing_mut().slider_width = ui.available_width().max(90.0);
                ui.add(egui::Slider::new(
                    &mut self.ppt_spl,
                    (spl.min as u8)..=(spl.max as u8),
                ).show_value(false));

                ui.add_space(6.0);
                slider_header(ui, "SPPT", &format!("{} W", self.ppt_sppt), ACCENT);
                ui.spacing_mut().slider_width = ui.available_width().max(90.0);
                ui.add(egui::Slider::new(
                    &mut self.ppt_sppt,
                    (sppt.min as u8)..=(sppt.max as u8),
                ).show_value(false));

                ui.add_space(6.0);
                slider_header(ui, "FPPT", &format!("{} W", self.ppt_fppt), ACCENT);
                ui.spacing_mut().slider_width = ui.available_width().max(90.0);
                ui.add_enabled(
                    state.limits.fppt_supported,
                    egui::Slider::new(&mut self.ppt_fppt, (fppt.min as u8)..=(fppt.max as u8)).show_value(false),
                );

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "Applica limiti").clicked() {
                        self.send(Command::SetPpt {
                            spl: self.ppt_spl,
                            sppt: self.ppt_sppt,
                            fppt: self.ppt_fppt,
                        });
                        self.notify("Limiti CPU applicati");
                    }
                    if secondary_button(ui, "Ripristina auto").clicked() {
                        self.ppt_spl = spl.min as u8;
                        self.ppt_sppt = sppt.min as u8;
                        self.ppt_fppt = fppt.min as u8;
                        self.send(Command::SetPpt { spl: 5, sppt: 5, fppt: 5 });
                        self.notify("Limiti CPU in auto");
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "Range modello · SPL {}–{} W · SPPT {}–{} W · FPPT {}–{} W",
                        spl.min, spl.max, sppt.min, sppt.max, fppt.min, fppt.max
                    ))
                    .size(9.0)
                    .color(MUTED),
                );
                ui.label(
                    RichText::new("APU SPPT non scrivibile su Linux (ENODEV).")
                        .size(9.0)
                        .color(MUTED),
                );
            });

            card(&mut cols[1], |ui| {
                ui.set_min_height(330.0);
                ui.horizontal(|ui| {
                    section_title(ui, "Limiti GPU");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        chip(
                            ui,
                            if state.gpu.offsets_supported { "NVML ONLINE" } else { "NVML ASSENTE" },
                            if state.gpu.offsets_supported { GREEN } else { YELLOW },
                        );
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Thermal target, Dynamic Boost e offset clock via WMI + NVML.")
                        .size(9.5)
                        .color(MUTED),
                );
                ui.add_space(12.0);
                let temp = state.limits.gpu_temp;
                let boost = state.limits.gpu_boost;
                let base = state.limits.gpu_base;
                let mem = state.limits.gpu_mem;

                slider_header(ui, "Thermal target", &format!("{} °C", self.nv_temp), ACCENT);
                ui.spacing_mut().slider_width = ui.available_width().max(90.0);
                ui.add(egui::Slider::new(
                    &mut self.nv_temp,
                    (temp.min as u8)..=(temp.max as u8),
                ).show_value(false));

                ui.add_space(6.0);
                slider_header(ui, "Dynamic Boost", &format!("{} W", self.nv_boost), ACCENT);
                ui.spacing_mut().slider_width = ui.available_width().max(90.0);
                ui.add(egui::Slider::new(
                    &mut self.nv_boost,
                    (boost.min as u8)..=(boost.max as u8),
                ).show_value(false));

                ui.add_space(6.0);
                slider_header(ui, "Base clock offset", &format!("{:+} MHz", self.gpu_base), if state.gpu.offsets_supported { ACCENT } else { MUTED });
                ui.spacing_mut().slider_width = ui.available_width().max(90.0);
                ui.add_enabled(
                    state.gpu.offsets_supported,
                    egui::Slider::new(&mut self.gpu_base, base.min..=base.max).show_value(false),
                );

                ui.add_space(6.0);
                slider_header(ui, "Memory clock offset", &format!("{:+} MHz", self.gpu_mem), if state.gpu.offsets_supported { ACCENT } else { MUTED });
                ui.spacing_mut().slider_width = ui.available_width().max(90.0);
                ui.add_enabled(
                    state.gpu.offsets_supported,
                    egui::Slider::new(&mut self.gpu_mem, mem.min..=mem.max).show_value(false),
                );

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "Applica limiti GPU").clicked() {
                        self.send(Command::SetNvLimits {
                            temp_target: self.nv_temp,
                            dynamic_boost: self.nv_boost,
                        });
                        if state.gpu.offsets_supported {
                            self.send(Command::SetGpuOffsets {
                                base_clock: self.gpu_base,
                                memory_clock: self.gpu_mem,
                            });
                        }
                        self.notify("Limiti GPU applicati");
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "Attuali · {} °C · {} W · offset {:+}/{:+} MHz",
                        state.gpu.nv_temp_target,
                        state.gpu.nv_dynamic_boost,
                        state.gpu.base_clock_offset,
                        state.gpu.memory_clock_offset
                    ))
                    .size(9.0)
                    .color(MUTED),
                );
                if !state.gpu.offsets_supported {
                    ui.label(
                        RichText::new("Offset clock non disponibili (NVML/GPU assente).")
                            .size(9.0)
                            .color(YELLOW),
                    );
                }
            });
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Modalità manuale");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut enabled = state.manual.enabled;
                    if switch(ui, &mut enabled) {
                        self.send(Command::SetManualEnabled { enabled });
                        self.notify(if enabled {
                            "Modalità manuale attiva"
                        } else {
                            "Modalità manuale disattivata (ventole auto)"
                        });
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if state.manual.enabled { "ATTIVA" } else { "DISATTIVATA" })
                            .size(9.0)
                            .color(if state.manual.enabled { GREEN } else { MUTED }),
                    );
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Salva curve ventole, PPT e limiti NVIDIA separatamente per AC e batteria. Il demone li applica al cambio di alimentazione.",
                )
                .size(10.0)
                .color(MUTED),
            );
            ui.add_space(12.0);
            ui.columns(2, |cols| {
                for (i, (label, profile, source_key)) in [
                    ("PROFILO AC", &state.manual.ac, "ac"),
                    ("PROFILO BATTERIA", &state.manual.battery, "battery"),
                ]
                .iter()
                .enumerate()
                {
                    Frame::new()
                        .fill(BG)
                        .stroke(Stroke::new(
                            1.0,
                            if state.manual.source == *source_key { with_alpha(ACCENT, 120) } else { BORDER },
                        ))
                        .corner_radius(CornerRadius::same(9))
                        .inner_margin(Margin::same(12))
                        .show(&mut cols[i], |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(*label).size(9.0).color(ACCENT).strong());
                                if state.manual.source == *source_key {
                                    chip(ui, "IN USO", GREEN);
                                }
                            });
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(format!(
                                    "SPL {} W · SPPT {} W · FPPT {} W",
                                    profile.spl, profile.sppt, profile.fppt
                                ))
                                .size(10.0)
                                .color(TEXT),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "GPU {} °C / {} W · curve 8 punti",
                                    profile.nv_temp_target, profile.nv_dynamic_boost
                                ))
                                .size(10.0)
                                .color(MUTED),
                            );
                            ui.add_space(8.0);
                            if secondary_button(ui, "Salva impostazioni correnti").clicked() {
                                self.save_manual(source_key);
                            }
                        });
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if primary_button(ui, "Applica profilo ora").clicked() {
                    self.send(Command::SetManualEnabled { enabled: true });
                    self.notify("Profilo manuale applicato");
                }
                ui.label(
                    RichText::new(format!(
                        "Sorgente attuale: {}",
                        if state.manual.source == "ac" { "rete elettrica" } else { "batteria" }
                    ))
                    .size(9.5)
                    .color(MUTED),
                );
            });
        });
    }

    fn save_manual(&mut self, source: &str) {
        for draft in &self.fans {
            if let Err(e) = draft.validate() {
                self.toast = Some(Toast { text: e, error: true, at: Instant::now() });
                return;
            }
        }
        self.send(Command::SaveManualProfile {
            source: source.to_string(),
            cpu: self.fans[0].points(),
            gpu: self.fans[1].points(),
            nv_temp_target: self.nv_temp,
            nv_dynamic_boost: self.nv_boost,
            spl: self.ppt_spl,
            sppt: self.ppt_sppt,
            fppt: self.ppt_fppt,
        });
        self.notify(if source == "ac" {
            "Profilo manuale AC salvato"
        } else {
            "Profilo manuale batteria salvato"
        });
    }

    fn battery_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Batteria", "Stato, consumo e limite di ricarica persistente");
        ui.add_space(16.0);

        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.set_min_height(168.0);
                section_title(ui, "Carica");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ring_gauge(
                        ui,
                        112.0,
                        state.battery.capacity as f32 / 100.0,
                        if state.battery.capacity >= 20 { GREEN } else { RED },
                        &format!("{}%", state.battery.capacity),
                        "capacità",
                    );
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(battery_status_label(&state.battery.status)).size(11.0).color(TEXT));
                        ui.add_space(2.0);
                        chip(
                            ui,
                            if state.battery.ac_online { "RETE ELETTRICA" } else { "A BATTERIA" },
                            if state.battery.ac_online { GREEN } else { YELLOW },
                        );
                    });
                });
            });

            card(&mut cols[1], |ui| {
                ui.set_min_height(168.0);
                section_title(ui, "Consumo");
                ui.add_space(10.0);
                metric(
                    ui,
                    "Potenza assorbita",
                    &format!("{:.1}", state.battery.power_now_w),
                    "W",
                    TEXT,
                );
                ui.add_space(10.0);
                egui::Grid::new("battery_consumo")
                    .num_columns(2)
                    .spacing([18.0, 6.0])
                    .show(ui, |ui| {
                        row(ui, "Stato", &battery_status_label(&state.battery.status));
                        row(
                            ui,
                            "Sorgente",
                            if state.battery.ac_online {
                                "rete elettrica"
                            } else {
                                "batteria"
                            },
                        );
                        row(ui, "Limite", &format!("{}%", state.battery.charge_limit));
                    });
            });
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Limite di ricarica");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    chip(ui, "PERSISTENTE", GREEN);
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("Ridurre la soglia aumenta la longevità della batteria. 100% = ricarica completa.")
                    .size(10.5)
                    .color(MUTED),
            );
            ui.add_space(14.0);

            let mut limited = self.charge_limit < 100;
            if toggle_row(
                ui,
                "Limita la ricarica",
                "Il demone ripristina questa soglia a ogni avvio.",
                &mut limited,
            ) {
                self.charge_limit = if limited { 80 } else { 100 };
                self.send(Command::SetChargeLimit { value: self.charge_limit });
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}%", self.charge_limit))
                        .font(logo(34.0))
                        .color(ACCENT),
                );
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("SOGLIA ATTUALE").size(8.5).color(MUTED));
                    ui.label(
                        RichText::new(if self.charge_limit < 100 {
                            "ricarica parziale"
                        } else {
                            "ricarica completa"
                        })
                        .size(10.0)
                        .color(TEXT),
                    );
                });
            });
            ui.add_space(6.0);
            ui.spacing_mut().slider_width = ui.available_width().max(120.0);
            let resp = ui.add_enabled(
                limited,
                egui::Slider::new(&mut self.charge_limit, 40..=100).show_value(false)
                    .step_by(5.0),
            );
            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                self.send(Command::SetChargeLimit { value: self.charge_limit });
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                for v in [60u8, 80, 90, 100] {
                    let selected = state.battery.charge_limit == v;
                    let response = ui.add_sized(
                        [72.0, 36.0],
                        egui::Button::new(
                            RichText::new(format!("{v}%"))
                                .size(11.0)
                                .color(if selected { TEXT } else { MUTED }),
                        )
                        .fill(if selected { ACCENT_SOFT } else { PANEL_HI })
                        .stroke(Stroke::new(
                            if selected { 1.2 } else { 1.0 },
                            if selected { ACCENT } else { BORDER },
                        ))
                        .corner_radius(CornerRadius::same(8)),
                    );
                    if response.clicked() {
                        self.charge_limit = v;
                        self.send(Command::SetChargeLimit { value: v });
                    }
                }
            });
        });
    }

    fn gpu_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "GPU e display", "Modalità grafica, luminosità del pannello e statistiche NVIDIA");
        ui.add_space(14.0);

        if state.gpu.pending_reboot {
            Frame::new()
                .fill(mix(PANEL, YELLOW, 0.10))
                .stroke(Stroke::new(1.0, with_alpha(YELLOW, 120)))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        paint_icon(ui.painter(), rect, YELLOW, Icon::Power);
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new("Riavvio richiesto per applicare le modifiche GPU/BIOS.")
                                .size(11.0)
                                .color(YELLOW)
                                .strong(),
                        );
                    });
                });
            ui.add_space(12.0);
        }

        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Modalità GPU");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    chip(
                        ui,
                        &format!(
                            "MUX {}",
                            if state.gpu.mux_mode == 0 { "ULTIMATE" } else { "HYBRID" }
                        ),
                        if state.gpu.mux_mode == 0 { ACCENT } else { GREEN },
                    );
                    chip(
                        ui,
                        if state.gpu.dgpu_disabled { "dGPU OFF" } else { "dGPU ON" },
                        if state.gpu.dgpu_disabled { YELLOW } else { GREEN },
                    );
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("Ultimate e Eco richiedono un riavvio. MUX 0 = solo dGPU, 1 = Optimus/Hybrid.")
                    .size(10.0)
                    .color(MUTED),
            );
            ui.add_space(12.0);
            let gap = 6.0;
            let width = ((ui.available_width() - gap * 3.0) / 4.0).max(150.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (key, title, desc) in GPU_MODES.iter() {
                    let icon = match *key {
                        "ultimate" => Icon::Gpu,
                        "standard" => Icon::Layers,
                        "eco" => Icon::Leaf,
                        _ => Icon::Sparkle,
                    };
                    let selected = state.gpu.mode == *key;
                    if mode_tile(ui, width, 74.0, icon, title, desc, selected).clicked() && !selected
                    {
                        self.confirm = Some(ConfirmAction::GpuMode(key.to_string()));
                    }
                }
            });
        });

        ui.add_space(14.0);
        if let Some(n) = state.nvidia.clone() {
            ui.columns(2, |cols| {
                self.display_card(&mut cols[0], state);
                self.nvidia_card(&mut cols[1], &n);
            });
        } else {
            self.display_card(ui, state);
        }
    }

    fn display_card(&mut self, ui: &mut egui::Ui, state: &State) {
        card(ui, |ui| {
            ui.set_min_height(190.0);
            ui.horizontal(|ui| {
                section_title(ui, "Display");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(info) = &self.display {
                        chip(ui, &format!("{:.0} HZ", info.current), GREEN);
                    }
                });
            });
            ui.add_space(10.0);
            if let Some(bl) = &state.backlight {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}", self.display_brightness))
                            .font(logo(40.0))
                            .color(ACCENT),
                    );
                    ui.vertical(|ui| {
                        ui.label(RichText::new("LUMINOSITÀ").size(8.5).color(MUTED));
                        ui.label(
                            RichText::new(format!("max {}", bl.max_brightness))
                                .size(10.0)
                                .color(TEXT),
                        );
                    });
                });
                ui.add_space(4.0);
                ui.spacing_mut().slider_width = ui.available_width().max(110.0);
                let resp = ui.add(
                    egui::Slider::new(&mut self.display_brightness, 0..=bl.max_brightness)
                        .show_value(false),
                );
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                    self.send(Command::SetDisplayBrightness {
                        value: self.display_brightness,
                    });
                }
            } else {
                ui.label(
                    RichText::new("Backlight non controllabile.")
                        .size(11.0)
                        .color(MUTED),
                );
            }
            ui.add_space(10.0);
            if let Some(info) = &self.display {
                Frame::new()
                    .fill(BG)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        egui::Grid::new("display_info")
                            .num_columns(2)
                            .spacing([18.0, 5.0])
                            .show(ui, |ui| {
                                row(ui, "Output", &info.output);
                                row(ui, "Modalità", &info.mode);
                                row(
                                    ui,
                                    "Refresh",
                                    &format!(
                                        "{:.0} Hz · range {:.0}–{:.0} Hz",
                                        info.current, info.min, info.max
                                    ),
                                );
                            });
                    });
            }
        });
    }

    fn nvidia_card(&mut self, ui: &mut egui::Ui, n: &NvidiaInfo) {
        card(ui, |ui| {
            ui.set_min_height(190.0);
            ui.horizontal(|ui| {
                section_title(ui, "NVIDIA");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    chip(ui, &format!("{:.0} W", n.power_w), YELLOW);
                });
            });
            ui.add_space(2.0);
            ui.label(RichText::new(&n.name).size(9.5).color(MUTED));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                metric(ui, "Core", &format!("{:.0}", n.clock_mhz), "MHz", TEXT);
                ui.add_space(18.0);
                metric(ui, "Memoria", &format!("{:.0}", n.mem_clock_mhz), "MHz", TEXT);
                ui.add_space(18.0);
                metric(ui, "Temp", &format!("{:.0}", n.temp_c), "°C", temp_color(n.temp_c));
            });
            ui.add_space(10.0);
            meter(
                ui,
                "Utilizzo",
                &format!("{:.0} %", n.util_pct),
                n.util_pct / 100.0,
                GREEN,
            );
            meter(
                ui,
                "Potenza",
                &format!("{:.0} / {:.0} W", n.power_w, n.max_power_limit_w.max(1.0)),
                n.power_w / n.max_power_limit_w.max(1.0),
                YELLOW,
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "VRAM {:.1} / {:.1} GB  ·  TGP max {:.0} W",
                    n.mem_used_mb / 1024.0,
                    n.mem_total_mb / 1024.0,
                    n.max_power_limit_w
                ))
                .size(9.5)
                .color(MUTED),
            );
        });
    }

    fn system_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Sistema", "Configurazione hardware, sensori e stato del demone");
        ui.add_space(16.0);

        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.set_min_height(360.0);
                section_title(ui, "Configurazione");
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Impostazioni hardware e della sessione desktop.")
                        .size(10.0)
                        .color(MUTED),
                );
                ui.add_space(10.0);

                if let Some(mut boost) = state.system.cpu_boost {
                    if toggle_row(
                        ui,
                        "CPU Boost",
                        "Permette alla CPU di superare la frequenza base.",
                        &mut boost,
                    ) {
                        self.send(Command::SetCpuBoost { value: boost });
                        self.notify(if boost {
                            "CPU Boost attivato"
                        } else {
                            "CPU Boost disattivato"
                        });
                    }
                    hr(ui);
                }
                let mut sound = state.gpu.boot_sound;
                if toggle_row(
                    ui,
                    "Suono di avvio ASUS",
                    "Beep del firmware all'accensione.",
                    &mut sound,
                ) {
                    self.send(Command::SetBootSound { value: sound });
                }
                hr(ui);
                let mut od = state.gpu.panel_overdrive;
                if toggle_row(
                    ui,
                    "Panel Overdrive",
                    "Riduce il tempo di risposta del pannello.",
                    &mut od,
                ) {
                    self.send(Command::SetPanelOverdrive { value: od });
                }
                hr(ui);
                if toggle_row(
                    ui,
                    "Touchpad",
                    "Abilita o disabilita il touchpad (X11).",
                    &mut self.touchpad_enabled,
                ) {
                    let desired = self.touchpad_enabled;
                    if let Err(e) = set_touchpad(desired) {
                        self.touchpad_enabled = !desired;
                        self.toast = Some(Toast { text: e, error: true, at: Instant::now() });
                    } else {
                        self.notify(if desired {
                            "Touchpad abilitato"
                        } else {
                            "Touchpad disabilitato"
                        });
                    }
                    store_session(&SessionConfig {
                        screen_saver: self.screen_saver,
                        touchpad_enabled: self.touchpad_enabled,
                        win_key_enabled: self.win_key_enabled,
                    });
                }
                hr(ui);
                if toggle_row(
                    ui,
                    "Tasto Windows",
                    "Blocca il tasto Super (solo X11).",
                    &mut self.win_key_enabled,
                ) {
                    let desired = self.win_key_enabled;
                    if let Err(e) = set_win_key(desired) {
                        self.win_key_enabled = !desired;
                        self.toast = Some(Toast { text: e, error: true, at: Instant::now() });
                    } else {
                        self.notify(if desired {
                            "Tasto Windows abilitato"
                        } else {
                            "Tasto Windows disabilitato"
                        });
                    }
                    store_session(&SessionConfig {
                        screen_saver: self.screen_saver,
                        touchpad_enabled: self.touchpad_enabled,
                        win_key_enabled: self.win_key_enabled,
                    });
                }
                hr(ui);
                if toggle_row(
                    ui,
                    "Risparmio energetico schermo",
                    "60 Hz a batteria, refresh massimo con l'alimentatore.",
                    &mut self.screen_saver,
                ) {
                    let enabled = self.screen_saver;
                    store_session(&SessionConfig {
                        screen_saver: enabled,
                        touchpad_enabled: self.touchpad_enabled,
                        win_key_enabled: self.win_key_enabled,
                    });
                    if enabled {
                        self.apply_screen_policy(state.battery.ac_online);
                    }
                    self.notify(if enabled {
                        "Risparmio schermo attivo"
                    } else {
                        "Risparmio schermo disattivato"
                    });
                }
                if let Some(info) = &self.display {
                    ui.add_space(8.0);
                    chip(
                        ui,
                        &format!(
                            "{} · {:.0} Hz · range {:.0}–{:.0} Hz",
                            info.output, info.current, info.min, info.max
                        ),
                        MUTED,
                    );
                }
            });

            card(&mut cols[1], |ui| {
                ui.set_min_height(360.0);
                section_title(ui, "Piattaforma");
                ui.add_space(8.0);
                egui::Grid::new("sys_grid")
                    .num_columns(2)
                    .spacing([24.0, 7.0])
                    .show(ui, |ui| {
                        row(ui, "Modello", &state.system.model);
                        row(ui, "Scheda madre", &state.system.board);
                        row(ui, "Kernel", &state.system.kernel);
                        row(ui, "Distribuzione", &state.system.os);
                        row(ui, "Hostname", &state.system.hostname);
                        row(ui, "Uptime", &format_uptime(state.system.uptime_secs));
                        row(ui, "Load 1m", &format!("{:.2}", state.system.load1));
                        row(ui, "Governor", &state.system.cpu_governor);
                        row(
                            ui,
                            "Profilo limiti",
                            &format!("{} ({})", state.limits.profile, state.limits.source),
                        );
                        row(ui, "Demone", &state.system.daemon_version);
                        row(ui, "Socket", &state.system.socket);
                    });
            });
        });

        ui.add_space(14.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.set_min_height(250.0);
                section_title(ui, "Temperature");
                ui.add_space(8.0);
                egui::Grid::new("temps_grid")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        for t in &state.temps {
                            ui.label(RichText::new(&t.label).size(11.0).color(MUTED));
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(5.0, 5.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(rect.center(), 2.5, temp_color(t.celsius));
                                ui.label(
                                    RichText::new(format!("{:.1} °C", t.celsius))
                                        .size(11.0)
                                        .monospace()
                                        .color(temp_color(t.celsius)),
                                );
                            });
                            ui.end_row();
                        }
                    });
            });

            card(&mut cols[1], |ui| {
                ui.set_min_height(250.0);
                section_title(ui, "Ventole e demone");
                ui.add_space(8.0);
                for fan in &state.fans {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&fan.label).size(10.0).color(MUTED));
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{}", fan.rpm))
                                        .font(logo(21.0))
                                        .color(ACCENT),
                                );
                                ui.label(RichText::new("RPM").size(9.0).color(MUTED));
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            chip(
                                ui,
                                if fan.custom { "CUSTOM" } else { "AUTO" },
                                if fan.custom { YELLOW } else { GREEN },
                            );
                        });
                    });
                }
                hr(ui);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("DEMONE").size(8.5).color(MUTED));
                        ui.horizontal(|ui| {
                            let ok = self.error.is_none() && self.state.is_some();
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(7.0, 7.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().circle_filled(
                                rect.center(),
                                3.5,
                                if ok { GREEN } else { RED },
                            );
                            ui.label(
                                RichText::new(if ok { "Connesso e operativo" } else { "Non connesso" })
                                    .size(10.5)
                                    .color(if ok { GREEN } else { RED }),
                            );
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if secondary_button(ui, "Riavvia demone").clicked() {
                            let _ = std::process::Command::new("pkexec")
                                .args(["systemctl", "restart", "rog-control-daemon"])
                                .spawn();
                            self.notify("Riavvio del demone richiesto a polkit");
                        }
                    });
                });
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.last_poll.elapsed() >= Duration::from_secs(2) {
            self.refresh();
            self.last_poll = Instant::now();
            let ac = self.state.as_ref().map(|s| s.battery.ac_online);
            if let Some(ac) = ac {
                if self.last_ac != Some(ac) {
                    self.last_ac = Some(ac);
                    self.apply_screen_policy(ac);
                }
            }
        }
        if self.tab == Tab::System && self.last_session.elapsed() >= Duration::from_secs(2) {
            self.last_session = Instant::now();
            self.sync_session_info();
        }
        // Le scritture sysfs sono immediate; un repaint periodico tiene vivi i sensori.
        ctx.request_repaint_after(Duration::from_secs(1));

        self.top_bar(ui);
        self.sidebar(ui);

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(22, 0)),
            )
            .show(ui, |ui| {
            paint_background(ui);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(14.0);
                    self.error_banner(ui);
                    let Some(state) = self.state.clone() else {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("In attesa del demone…").size(20.0).weak());
                        });
                        return;
                    };
                    match self.tab {
                        Tab::Home => self.home_page(ui, &state),
                        Tab::Keyboard => self.keyboard_page(ui, &state),
                        Tab::Fans => self.fans_page(ui, &state),
                        Tab::Power => self.power_page(ui, &state),
                        Tab::Battery => self.battery_page(ui, &state),
                        Tab::Gpu => self.gpu_page(ui, &state),
                        Tab::System => self.system_page(ui, &state),
                    }
                    ui.add_space(20.0);
                });
        });

        self.toast(&ctx);
        self.confirm_modal(&ctx);
    }
}

fn configure_style(ctx: &egui::Context) {
    // Font "black" dedicato a titoli, numeri e wordmark ROG (SIL OFL, in assets/).
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "archivo_black".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/ArchivoBlack-Regular.ttf"
        ))),
    );
    fonts.families.insert(
        egui::FontFamily::Name("Logo".into()),
        vec!["archivo_black".to_owned()],
    );
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let mut v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = PANEL_HI;
        v.window_stroke = Stroke::new(1.0, BORDER_HI);
        v.extreme_bg_color = Color32::from_rgb(9, 9, 12);
        v.faint_bg_color = PANEL;
        v.override_text_color = None;
        v.selection.bg_fill = Color32::from_rgba_unmultiplied(255, 42, 78, 70);
        v.selection.stroke = Stroke::new(1.0, ACCENT);

        v.widgets.noninteractive.bg_fill = PANEL;
        v.widgets.noninteractive.weak_bg_fill = PANEL;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
        v.widgets.noninteractive.corner_radius = CornerRadius::same(8);

        v.widgets.inactive.bg_fill = PANEL_HI;
        v.widgets.inactive.weak_bg_fill = PANEL_HI;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(206, 208, 218));
        v.widgets.inactive.corner_radius = CornerRadius::same(8);

        v.widgets.hovered.bg_fill = Color32::from_rgb(38, 20, 28);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(38, 20, 28);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.hovered.corner_radius = CornerRadius::same(8);

        v.widgets.active.bg_fill = ACCENT_SOFT;
        v.widgets.active.weak_bg_fill = ACCENT_SOFT;
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.active.corner_radius = CornerRadius::same(8);

        v.slider_trailing_fill = true;
        style.visuals = v;

        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.slider_width = 170.0;
        style.spacing.interact_size = egui::vec2(40.0, 24.0);
        style.spacing.indent = 18.0;
        style.text_styles.insert(TextStyle::Heading, FontId::new(26.0, egui::FontFamily::Name("Logo".into())));
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(13.0));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        style.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.0));
    });
}

fn logo(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("Logo".into()))
}

fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn slider_header(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 18.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.text(
        egui::pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.0),
        MUTED,
    );
    painter.text(
        egui::pos2(rect.right() - 2.0, rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        logo(13.0),
        color,
    );
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).font(logo(15.0)).color(TEXT));
}

fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("ROG CONTROL")
                        .size(9.0)
                        .color(ACCENT)
                        .strong(),
                );
                ui.label(RichText::new("//").size(9.0).color(MUTED));
                ui.label(
                    RichText::new(title.to_uppercase())
                        .size(9.0)
                        .color(MUTED),
                );
            });
            ui.add_space(1.0);
            ui.label(RichText::new(title).font(logo(30.0)).color(TEXT));
            ui.label(RichText::new(subtitle).size(12.5).color(MUTED));
        });
    }
}

fn row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).size(11.5).color(MUTED));
    ui.label(RichText::new(value).size(11.5).monospace().color(TEXT));
    ui.end_row();
}

fn metric(ui: &mut egui::Ui, label: &str, value: &str, unit: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(10.0).color(MUTED));
        ui.horizontal(|ui| {
            ui.label(RichText::new(value).font(logo(24.0)).color(color));
            if !unit.is_empty() {
                ui.label(RichText::new(unit).size(10.5).color(MUTED));
            }
        });
    });
}

fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(with_alpha(color, 24))
        .stroke(Stroke::new(1.0, with_alpha(color, 90)))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.0).color(color));
        });
}

fn meter(ui: &mut egui::Ui, label: &str, value: &str, fraction: f32, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(10.5).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(10.5).monospace().color(TEXT));
        });
    });
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), TRACK);
    let w = (rect.width() * fraction.clamp(0.0, 1.0)).max(0.0);
    if w > 1.0 {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(w, rect.height())),
            CornerRadius::same(3),
            color,
        );
    }
    ui.add_space(2.0);
}

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).size(12.5).strong().color(Color32::WHITE))
            .fill(ACCENT)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .min_size(egui::vec2(0.0, 34.0)),
    )
}

fn secondary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).size(12.0).color(TEXT))
            .fill(PANEL_HI)
            .stroke(Stroke::new(1.0, BORDER_HI))
            .corner_radius(CornerRadius::same(8))
            .min_size(egui::vec2(0.0, 32.0)),
    )
}

fn icon_button(ui: &mut egui::Ui, icon: Icon) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::click());
    let color = if resp.hovered() { TEXT } else { MUTED };
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), PANEL_HI);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0, BORDER_HI),
            egui::StrokeKind::Inside,
        );
    }
    paint_icon(ui.painter(), rect.shrink(9.0), color, icon);
    resp
}

fn switch(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(44.0, 24.0), egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let bg = mix(TRACK, ACCENT, t);
    ui.painter().rect_filled(rect, CornerRadius::same(12), bg);
    let cx = rect.left() + 12.0 + (rect.width() - 24.0) * t;
    ui.painter()
        .circle_filled(egui::pos2(cx, rect.center().y), 8.0, Color32::WHITE);
    resp.changed()
}

fn toggle_row(ui: &mut egui::Ui, title: &str, desc: &str, on: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(13.0).color(TEXT));
            if !desc.is_empty() {
                ui.label(RichText::new(desc).size(10.5).color(MUTED));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = switch(ui, on);
        });
    });
    changed
}

#[allow(clippy::too_many_arguments)]
fn mode_tile(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    icon: Icon,
    title: &str,
    desc: &str,
    selected: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let hovered = resp.hovered();
    let bg = if selected {
        mix(PANEL, ACCENT_SOFT, 0.85)
    } else if hovered {
        PANEL_HI
    } else {
        PANEL
    };
    let border = if selected {
        ACCENT
    } else if hovered {
        BORDER_HI
    } else {
        BORDER
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(12),
        bg,
        Stroke::new(if selected { 1.4 } else { 1.0 }, border),
        egui::StrokeKind::Inside,
    );
    let icon_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(14.0, 15.0),
        egui::vec2(30.0, 30.0),
    );
    painter.rect_filled(
        icon_rect,
        CornerRadius::same(8),
        if selected { with_alpha(ACCENT, 30) } else { TRACK },
    );
    paint_icon(
        painter,
        icon_rect.shrink(7.0),
        if selected { ACCENT } else { MUTED },
        icon,
    );
    let tx = icon_rect.right() + 12.0;
    painter.text(
        egui::pos2(tx, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        title,
        logo(13.0),
        if selected { TEXT } else { Color32::from_rgb(210, 212, 222) },
    );
    painter.text(
        egui::pos2(tx, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        desc,
        FontId::proportional(10.0),
        MUTED,
    );
    resp
}

fn nav_item(ui: &mut egui::Ui, icon: Icon, label: &str, selected: bool) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 42.0), egui::Sense::click());
    let hovered = resp.hovered();
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), ACCENT_SOFT);
    } else if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), PANEL_HI);
    }
    let color = if selected {
        ACCENT
    } else if hovered {
        TEXT
    } else {
        Color32::from_rgb(176, 178, 190)
    };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 25.0, rect.center().y),
        egui::vec2(17.0, 17.0),
    );
    paint_icon(ui.painter(), icon_rect, color, icon);
    ui.painter().text(
        egui::pos2(rect.left() + 46.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        color,
    );
    resp
}

fn hr(ui: &mut egui::Ui) {
    ui.add_space(7.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(1), BORDER);
    ui.add_space(7.0);
}

fn ring_gauge(
    ui: &mut egui::Ui,
    size: f32,
    fraction: f32,
    color: Color32,
    big: &str,
    small: &str,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let center = rect.center();
    let radius = size * 0.5 - 12.0;
    let painter = ui.painter();
    painter.circle_stroke(center, radius, Stroke::new(9.0, TRACK));

    let start = -std::f32::consts::FRAC_PI_2;
    let sweep = fraction.clamp(0.0, 1.0) * std::f32::consts::TAU;
    let points: Vec<egui::Pos2> = (0..=96)
        .map(|i| {
            let a = start + sweep * i as f32 / 96.0;
            center + egui::vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    if points.len() > 1 {
        painter.add(egui::Shape::line(points.clone(), Stroke::new(18.0, with_alpha(color, 26))));
        painter.add(egui::Shape::line(points, Stroke::new(9.0, color)));
    }
    painter.text(
        center + egui::vec2(0.0, -8.0),
        Align2::CENTER_CENTER,
        big,
        logo(size * 0.24),
        TEXT,
    );
    painter.text(
        center + egui::vec2(0.0, 20.0),
        Align2::CENTER_CENTER,
        small,
        FontId::proportional(10.0),
        MUTED,
    );
}

fn paint_background(ui: &egui::Ui) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    painter.text(
        egui::pos2(rect.right() - 18.0, rect.bottom() + 84.0),
        Align2::RIGHT_BOTTOM,
        "ROG",
        logo(340.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, 6),
    );
    for i in 0..4 {
        let y = rect.top() + rect.height() * (0.18 + i as f32 * 0.17);
        painter.line_segment(
            [
                egui::pos2(rect.left(), y),
                egui::pos2(rect.right(), y - 90.0),
            ],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 4)),
        );
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Icon {
    Home,
    Keyboard,
    Fan,
    Bolt,
    Battery,
    Gpu,
    Sliders,
    Refresh,
    Monitor,
    Palette,
    Memory,
    Disk,
    Power,
    Layers,
    Leaf,
    Sparkle,
    Moon,
    Speed,
    Grid,
    Shield,
}

fn paint_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32, icon: Icon) {
    use egui::{pos2, vec2, Shape};
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new(1.6, color);
    let thin = Stroke::new(1.3, color);
    match icon {
        Icon::Home => {
            painter.add(Shape::line(
                vec![pos2(c.x - r * 0.9, c.y - r * 0.05), pos2(c.x, c.y - r * 0.9), pos2(c.x + r * 0.9, c.y - r * 0.05)],
                stroke,
            ));
            painter.line_segment([pos2(c.x - r * 0.6, c.y - r * 0.1), pos2(c.x - r * 0.6, c.y + r * 0.75)], stroke);
            painter.line_segment([pos2(c.x + r * 0.6, c.y - r * 0.1), pos2(c.x + r * 0.6, c.y + r * 0.75)], stroke);
            painter.line_segment([pos2(c.x - r * 0.6, c.y + r * 0.75), pos2(c.x + r * 0.6, c.y + r * 0.75)], stroke);
        }
        Icon::Keyboard => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, vec2(r * 1.7, r * 1.15)),
                CornerRadius::same(3),
                stroke,
                egui::StrokeKind::Inside,
            );
            for i in -1..=1 {
                painter.circle_filled(pos2(c.x + i as f32 * r * 0.42, c.y - r * 0.1), 1.2, color);
            }
            painter.line_segment([pos2(c.x - r * 0.35, c.y + r * 0.32), pos2(c.x + r * 0.35, c.y + r * 0.32)], thin);
        }
        Icon::Fan => {
            painter.circle_stroke(c, r * 0.85, thin);
            for k in 0..4 {
                let a = k as f32 * std::f32::consts::FRAC_PI_2 + 0.4;
                let p = c + vec2(a.cos(), a.sin()) * r * 0.5;
                painter.line_segment([c, p], stroke);
                painter.circle_filled(p, 1.4, color);
            }
            painter.circle_filled(c, 2.0, color);
        }
        Icon::Bolt => {
            let pts = vec![
                pos2(c.x + r * 0.15, c.y - r * 0.95),
                pos2(c.x - r * 0.6, c.y + r * 0.12),
                pos2(c.x - r * 0.02, c.y + r * 0.12),
                pos2(c.x - r * 0.18, c.y + r * 0.95),
                pos2(c.x + r * 0.62, c.y - r * 0.16),
                pos2(c.x + r * 0.02, c.y - r * 0.16),
            ];
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Icon::Battery => {
            let body = egui::Rect::from_center_size(c - vec2(r * 0.1, 0.0), vec2(r * 1.55, r * 0.95));
            painter.rect_stroke(body, CornerRadius::same(3), stroke, egui::StrokeKind::Inside);
            painter.rect_filled(
                egui::Rect::from_min_size(body.min + vec2(3.0, 3.0), vec2((body.width() - 6.0) * 0.7, body.height() - 6.0)),
                CornerRadius::same(1),
                color,
            );
            painter.rect_filled(
                egui::Rect::from_center_size(pos2(body.right() + r * 0.16, c.y), vec2(r * 0.16, r * 0.4)),
                CornerRadius::same(1),
                color,
            );
        }
        Icon::Gpu => {
            let body = egui::Rect::from_center_size(c, vec2(r * 1.3, r * 1.3));
            painter.rect_stroke(body, CornerRadius::same(3), stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(
                body.shrink(r * 0.3),
                CornerRadius::same(2),
                thin,
                egui::StrokeKind::Inside,
            );
            for i in -1..=1 {
                let off = i as f32 * r * 0.38;
                painter.line_segment([pos2(body.left() - r * 0.2, c.y + off), pos2(body.left(), c.y + off)], thin);
                painter.line_segment([pos2(body.right(), c.y + off), pos2(body.right() + r * 0.2, c.y + off)], thin);
            }
        }
        Icon::Sliders => {
            for (i, w) in [0.7f32, 0.45, 0.8].iter().enumerate() {
                let y = c.y - r * 0.6 + i as f32 * r * 0.6;
                painter.line_segment([pos2(c.x - r * 0.9, y), pos2(c.x + r * 0.9, y)], thin);
                painter.circle_filled(pos2(c.x - r * 0.9 + r * 1.8 * w, y), 2.2, color);
            }
        }
        Icon::Refresh => {
            let pts: Vec<egui::Pos2> = (0..=40)
                .map(|i| {
                    let a = -2.2 + 4.2 * i as f32 / 40.0;
                    c + vec2(a.cos(), a.sin()) * r * 0.75
                })
                .collect();
            painter.add(Shape::line(pts, stroke));
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(c.x + r * 0.5, c.y - r * 0.75),
                    pos2(c.x + r * 0.95, c.y - r * 0.55),
                    pos2(c.x + r * 0.55, c.y - r * 0.3),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Monitor => {
            let body = egui::Rect::from_center_size(c + vec2(0.0, -r * 0.15), vec2(r * 1.8, r * 1.25));
            painter.rect_stroke(body, CornerRadius::same(3), stroke, egui::StrokeKind::Inside);
            painter.line_segment([pos2(c.x, body.bottom()), pos2(c.x, c.y + r * 0.65)], thin);
            painter.line_segment([pos2(c.x - r * 0.45, c.y + r * 0.65), pos2(c.x + r * 0.45, c.y + r * 0.65)], thin);
        }
        Icon::Palette => {
            painter.circle_stroke(c, r * 0.85, stroke);
            for (dx, dy, col) in [
                (-r * 0.35, -r * 0.25, ACCENT),
                (r * 0.05, -r * 0.45, YELLOW),
                (r * 0.45, r * 0.05, GREEN),
            ] {
                painter.circle_filled(c + vec2(dx, dy), r * 0.15, col);
            }
        }
        Icon::Memory => {
            let body = egui::Rect::from_center_size(c, vec2(r * 1.7, r * 1.0));
            painter.rect_stroke(body, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
            for i in -2..=2 {
                let x = c.x + i as f32 * r * 0.3;
                painter.line_segment([pos2(x, body.top()), pos2(x, body.top() - r * 0.2)], thin);
                painter.line_segment([pos2(x, body.bottom()), pos2(x, body.bottom() + r * 0.2)], thin);
            }
        }
        Icon::Disk => {
            painter.circle_stroke(c, r * 0.88, stroke);
            painter.circle_stroke(c, r * 0.28, thin);
            painter.circle_filled(c + vec2(r * 0.45, -r * 0.45), 1.6, color);
        }
        Icon::Power => {
            let pts: Vec<egui::Pos2> = (0..=30)
                .map(|i| {
                    let a = -2.3 + 4.6 * i as f32 / 30.0;
                    c + vec2(a.cos(), a.sin()) * r * 0.8
                })
                .collect();
            painter.add(Shape::line(pts, stroke));
            painter.line_segment([pos2(c.x, c.y - r * 0.95), pos2(c.x, c.y - r * 0.15)], stroke);
        }
        Icon::Layers => {
            painter.add(Shape::closed_line(
                vec![pos2(c.x, c.y - r * 0.8), pos2(c.x + r * 0.9, c.y - r * 0.2), pos2(c.x, c.y + r * 0.4), pos2(c.x - r * 0.9, c.y - r * 0.2)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![pos2(c.x - r * 0.9, c.y + r * 0.25), pos2(c.x, c.y + r * 0.85), pos2(c.x + r * 0.9, c.y + r * 0.25)],
                thin,
            ));
        }
        Icon::Leaf => {
            let pts: Vec<egui::Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    let a = std::f32::consts::PI * (0.15 + t * 0.85);
                    c + vec2((a.cos() - 0.2) * r, -(a.sin()) * r * 0.9)
                })
                .collect();
            painter.add(Shape::line(pts, stroke));
            painter.line_segment([pos2(c.x - r * 0.5, c.y + r * 0.7), pos2(c.x + r * 0.5, c.y - r * 0.6)], thin);
        }
        Icon::Sparkle => {
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(c.x, c.y - r * 0.95),
                    pos2(c.x + r * 0.24, c.y - r * 0.24),
                    pos2(c.x + r * 0.95, c.y),
                    pos2(c.x + r * 0.24, c.y + r * 0.24),
                    pos2(c.x, c.y + r * 0.95),
                    pos2(c.x - r * 0.24, c.y + r * 0.24),
                    pos2(c.x - r * 0.95, c.y),
                    pos2(c.x - r * 0.24, c.y - r * 0.24),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Moon => {
            let pts: Vec<egui::Pos2> = (0..=28)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 28.0;
                    c + vec2(a.cos(), a.sin()) * r * 0.85
                })
                .collect();
            painter.add(Shape::line(pts, stroke));
            painter.add(Shape::line(
                vec![pos2(c.x + r * 0.25, c.y - r * 0.8), pos2(c.x + r * 0.55, c.y), pos2(c.x + r * 0.25, c.y + r * 0.8)],
                stroke,
            ));
        }
        Icon::Speed => {
            let pts: Vec<egui::Pos2> = (0..=24)
                .map(|i| {
                    let a = std::f32::consts::PI + std::f32::consts::PI * i as f32 / 24.0;
                    c + vec2(a.cos(), a.sin()) * r * 0.85 + vec2(0.0, r * 0.25)
                })
                .collect();
            painter.add(Shape::line(pts, stroke));
            painter.line_segment([c + vec2(0.0, r * 0.25), c + vec2(r * 0.45, -r * 0.35)], Stroke::new(1.8, color));
            painter.circle_filled(c + vec2(0.0, r * 0.25), 2.2, color);
        }
        Icon::Grid => {
            for dx in [-1.0f32, 1.0] {
                for dy in [-1.0f32, 1.0] {
                    let rr = egui::Rect::from_center_size(
                        c + vec2(dx * r * 0.42, dy * r * 0.42),
                        vec2(r * 0.58, r * 0.58),
                    );
                    painter.rect_stroke(rr, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                }
            }
        }
        Icon::Shield => {
            painter.add(Shape::closed_line(
                vec![
                    pos2(c.x, c.y - r * 0.9),
                    pos2(c.x + r * 0.8, c.y - r * 0.55),
                    pos2(c.x + r * 0.65, c.y + r * 0.4),
                    pos2(c.x, c.y + r * 0.9),
                    pos2(c.x - r * 0.65, c.y + r * 0.4),
                    pos2(c.x - r * 0.8, c.y - r * 0.55),
                ],
                stroke,
            ));
        }
    }
}



fn pct_to_pwm(pct: u8) -> u8 {
    ((pct as u16 * 255 + 50) / 100) as u8
}

fn pwm_to_pct(pwm: u8) -> u8 {
    ((pwm as u16 * 100 + 127) / 255) as u8
}

fn temp_color(t: f32) -> Color32 {
    if t >= 90.0 {
        RED
    } else if t >= 75.0 {
        YELLOW
    } else {
        GREEN
    }
}

fn format_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    format!("{h}h {m}m")
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn aura_mode_name(mode: u8) -> &'static str {
    AURA_MODES
        .iter()
        .find(|(_, v)| *v == mode)
        .map(|(n, _)| *n)
        .unwrap_or("Statico")
}

fn speed_name(s: u8) -> &'static str {
    match s {
        0 => "Bassa",
        2 => "Alta",
        _ => "Media",
    }
}

fn direction_name(d: u8) -> &'static str {
    match d {
        1 => "Sinistra",
        2 => "Su",
        3 => "Giù",
        _ => "Destra",
    }
}

fn fan_graph(ui: &mut egui::Ui, draft: &FanDraft, current: Option<f32>) {
    let height = 142.0;
    let (response, painter) =
        ui.allocate_painter(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let rect = response.rect.shrink2(egui::vec2(34.0, 14.0));
    painter.rect_filled(response.rect, CornerRadius::same(9), BG);
    painter.rect_stroke(
        response.rect,
        CornerRadius::same(9),
        Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );

    let t_min = 20.0f32;
    let t_max = 110.0f32;
    let x_of = |t: f32| rect.left() + ((t - t_min) / (t_max - t_min)).clamp(0.0, 1.0) * rect.width();
    let y_of = |p: f32| rect.bottom() - (p / 100.0).clamp(0.0, 1.0) * rect.height();

    let grid = Color32::from_rgb(30, 30, 38);
    for t in [20.0f32, 40.0, 60.0, 80.0, 100.0] {
        let x = x_of(t);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0, grid),
        );
        painter.text(
            egui::pos2(x, rect.bottom() + 3.0),
            Align2::CENTER_TOP,
            format!("{t:.0}°"),
            FontId::proportional(8.0),
            MUTED,
        );
    }
    for p in [0.0f32, 25.0, 50.0, 75.0, 100.0] {
        let y = y_of(p);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(1.0, grid),
        );
        painter.text(
            egui::pos2(rect.left() - 5.0, y),
            Align2::RIGHT_CENTER,
            format!("{p:.0}%"),
            FontId::proportional(8.0),
            MUTED,
        );
    }

    let points: Vec<egui::Pos2> = (0..8)
        .map(|i| egui::pos2(x_of(draft.temps[i] as f32), y_of(draft.pct[i] as f32)))
        .collect();

    let mut area = points.clone();
    area.push(egui::pos2(points[7].x, rect.bottom()));
    area.push(egui::pos2(points[0].x, rect.bottom()));
    painter.add(egui::Shape::convex_polygon(
        area,
        with_alpha(ACCENT, 26),
        Stroke::NONE,
    ));

    painter.add(egui::Shape::line(
        points.clone(),
        Stroke::new(5.0, with_alpha(ACCENT, 38)),
    ));
    painter.add(egui::Shape::line(points.clone(), Stroke::new(2.0, ACCENT)));
    for p in &points {
        painter.circle_filled(*p, 4.0, BG);
        painter.circle_stroke(*p, 3.2, Stroke::new(1.8, ACCENT));
    }

    if let Some(t) = current {
        let x = x_of(t);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0, with_alpha(YELLOW, 170)),
        );
        painter.text(
            egui::pos2(x + 5.0, rect.top() + 2.0),
            Align2::LEFT_TOP,
            format!("{t:.0}°C"),
            FontId::proportional(9.0),
            YELLOW,
        );
    }
}

const OPERATING_MODES: &[(&str, &str, &str)] = &[
    ("windows", "Windows", "Controllo del sistema"),
    ("silent", "Silenziosa", "Ventole lente, limiti ridotti"),
    ("performance", "Prestazioni", "Bilanciata"),
    ("turbo", "Turbo", "Massima potenza"),
    ("manual", "Manuale", "Curve e limiti custom"),
];

fn mode_title(mode: &str) -> &'static str {
    match mode {
        "windows" => "Windows",
        "silent" => "Silenziosa",
        "performance" => "Prestazioni",
        "turbo" => "Turbo",
        "manual" => "Manuale",
        _ => "Windows",
    }
}

fn mode_desc(mode: &str) -> &'static str {
    match mode {
        "windows" => "Gestione automatica del sistema, bilanciata tra potenza e silenzio.",
        "silent" => "Ventole lente e limiti ridotti per il massimo silenzio.",
        "performance" => "Profilo bilanciato per uso quotidiano e gaming leggero.",
        "turbo" => "Massima potenza CPU e GPU, ventole aggressive.",
        "manual" => "Curve ventole e limiti personalizzati per AC e batteria.",
        _ => "Profilo gestito dal firmware ASUS.",
    }
}

fn mode_desc_short(mode: &str) -> &'static str {
    match mode {
        "windows" => "Automatico",
        "silent" => "Silenzio",
        "performance" => "Bilanciata",
        "turbo" => "Massima",
        "manual" => "Custom",
        _ => "Firmware",
    }
}

fn battery_status_label(status: &str) -> String {
    match status.trim().to_lowercase().as_str() {
        "charging" => "In carica".to_string(),
        "discharging" => "In scarica".to_string(),
        "full" => "Carica completa".to_string(),
        "not charging" => "Non in carica".to_string(),
        "unknown" => "Sconosciuto".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => "—".to_string(),
            }
        }
    }
}

fn gpu_mode_title(mode: &str) -> &'static str {
    match mode {
        "ultimate" => "Ultimate",
        "standard" => "Standard",
        "eco" => "Eco",
        "optimized" => "Ottimizzato",
        _ => "Standard",
    }
}

const GPU_MODES: &[(&str, &str, &str)] = &[
    ("ultimate", "Ultimate", "Solo dGPU, massime prestazioni"),
    ("standard", "Standard", "Hybrid / Optimus"),
    ("eco", "Eco", "dGPU off, massima autonomia"),
    ("optimized", "Ottimizzato", "Hybrid con risparmio automatico"),
];

const AURA_MODES: &[(&str, u8)] = &[
    ("Statico", 0),
    ("Respiro", 1),
    ("Stroboscopica", 12),
    ("Ciclo colore", 2),
    ("Arcobaleno", 3),
    ("Cometa", 11),
    ("Flash & Dash", 10),
    ("Stelle", 4),
    ("Pioggia", 5),
    ("Reattivo", 8),
    ("Laser", 7),
    ("Onda", 6),
];
