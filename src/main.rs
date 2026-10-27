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

const ACCENT: Color32 = Color32::from_rgb(255, 72, 84);
const ACCENT_SOFT: Color32 = Color32::from_rgb(120, 32, 40);
const GREEN: Color32 = Color32::from_rgb(92, 214, 132);
const YELLOW: Color32 = Color32::from_rgb(240, 196, 80);
const RED: Color32 = Color32::from_rgb(255, 96, 96);
const PANEL: Color32 = Color32::from_rgb(22, 22, 28);
const BG: Color32 = Color32::from_rgb(15, 15, 19);

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
    nv_temp: u8,
    nv_boost: u8,
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
            nv_temp: 75,
            nv_boost: 5,
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
        self.ppt_spl = state.ppt.spl;
        self.ppt_sppt = state.ppt.sppt;
        self.nv_temp = state.gpu.nv_temp_target;
        self.nv_boost = state.gpu.nv_dynamic_boost;
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
            .exact_size(48.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(8.0);
                    let (color, label) = if self.error.is_some() {
                        (RED, "Demone offline")
                    } else if self.state.is_some() {
                        (GREEN, "Connesso")
                    } else {
                        (YELLOW, "Connessione…")
                    };
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 5.0, color);
                    ui.label(RichText::new(label).size(12.0).color(color));
                    ui.add_space(12.0);

                    if let Some(state) = &self.state {
                        ui.label(RichText::new(&state.system.model).size(14.0).strong());
                        ui.label(
                            RichText::new(format!("kernel {}", state.system.kernel))
                                .size(11.0)
                                .weak(),
                        );
                    } else {
                        ui.label(RichText::new("ROG Control").strong());
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        if ui.button("Aggiorna").clicked() {
                            self.refresh();
                        }
                        if let Some(t) = self.cpu_temp() {
                            ui.label(
                                RichText::new(format!("CPU {t:.0} °C"))
                                    .size(12.0)
                                    .color(temp_color(t)),
                            );
                        }
                        if let Some(t) = self.gpu_temp() {
                            ui.label(
                                RichText::new(format!("GPU {t:.0} °C"))
                                    .size(12.0)
                                    .color(temp_color(t)),
                            );
                        }
                    });
                });
            });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .exact_size(206.0)
            .resizable(false)
            .show(ui, |ui| {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("ROG")
                            .font(egui::FontId::new(
                                54.0,
                                egui::FontFamily::Name("Logo".into()),
                            ))
                            .color(ACCENT),
                    );
                    ui.add_space(2.0);
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(78.0, 3.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(rect, CornerRadius::same(2), ACCENT);
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("C O N T R O L")
                            .size(11.0)
                            .color(Color32::from_rgb(160, 160, 172)),
                    );
                });
                ui.add_space(20.0);
                ui.separator();
                ui.add_space(8.0);

                for tab in [
                    Tab::Home,
                    Tab::Keyboard,
                    Tab::Fans,
                    Tab::Power,
                    Tab::Battery,
                    Tab::Gpu,
                    Tab::System,
                ] {
                    let selected = self.tab == tab;
                    let text = RichText::new(format!("  {}", tab.label()))
                        .size(15.0)
                        .color(if selected { ACCENT } else { Color32::from_rgb(210, 210, 220) });
                    if ui
                        .add_sized(
                            [186.0, 38.0],
                            egui::Button::new(text).selected(selected).frame(false),
                        )
                        .clicked()
                    {
                        self.tab = tab;
                    }
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!("v{}", DAEMON_VERSION))
                            .size(10.0)
                            .weak(),
                    );
                    ui.label(
                        RichText::new("ASUS ROG hardware control")
                            .size(10.0)
                            .weak(),
                    );
                });
            });
    }

    fn error_banner(&mut self, ui: &mut egui::Ui) {
        let Some(err) = self.error.clone() else { return };
        Frame::default()
            .fill(Color32::from_rgb(58, 20, 26))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Demone non raggiungibile").strong().color(RED));
                    ui.label(RichText::new(err).size(11.0).weak());
                });
                ui.horizontal(|ui| {
                    if ui.button("Riprova").clicked() {
                        self.refresh();
                    }
                    if ui.button("Avvia demone").clicked() {
                        let _ = std::process::Command::new("pkexec")
                            .args(["systemctl", "start", "rog-control-daemon"])
                            .spawn();
                        self.notify("Richiesta di avvio inviata a polkit…");
                    }
                    ui.label(
                        RichText::new("Installa con ./install.sh se il servizio non esiste")
                            .size(11.0)
                            .weak(),
                    );
                });
            });
        ui.add_space(10.0);
    }

    fn toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else { return };
        if toast.at.elapsed() > Duration::from_secs(5) {
            self.toast = None;
            return;
        }
        let color = if toast.error { RED } else { GREEN };
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-18.0, -18.0))
            .interactable(false)
            .show(ctx, |ui| {
                Frame::default()
                    .fill(Color32::from_rgb(28, 28, 34))
                    .stroke(Stroke::new(1.0, color))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&toast.text).size(13.0).color(color));
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
                ui.set_width(360.0);
                match &action {
                    ConfirmAction::GpuMode(mode) => {
                        let (title, detail) = match mode.as_str() {
                            "ultimate" => (
                                "Passare a Ultimate (solo dGPU)?",
                                "Massime prestazioni video, consumi più alti. Richiede un riavvio.",
                            ),
                            "eco" => (
                                "Passare a Eco (dGPU disattivata)?",
                                "Massima autonomia e meno calore; la GPU NVIDIA non sarà disponibile. Richiede un riavvio.",
                            ),
                            "optimized" => (
                                "Passare a Ottimizzato?",
                                "Hybrid/Optimus con gestione dinamica della dGPU da parte di Linux. Potrebbe richiedere un riavvio se cambia il mux.",
                            ),
                            _ => (
                                "Passare a Standard (Hybrid/Optimus)?",
                                "La dGPU resta disponibile su richiesta. Richiede un riavvio se cambia il mux.",
                            ),
                        };
                        ui.label(RichText::new(title).strong());
                        ui.add_space(6.0);
                        ui.label(detail);
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Annulla").clicked() {
                        self.confirm = None;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("Conferma").strong()).fill(ACCENT))
                        .clicked()
                    {
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
        if !open {
            self.confirm = None;
        }
    }

    fn home_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Home", "Panoramica, modalità operative e statistiche");
        ui.add_space(12.0);

        card(ui, |ui| {
            ui.label(RichText::new("Modalità operativa").size(16.0).strong());
            ui.label(
                RichText::new("Profili del firmware, come in Armoury Crate.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for (key, title, desc) in OPERATING_MODES {
                    let selected = state.operating_mode == *key;
                    let fill = if selected { ACCENT_SOFT } else { Color32::from_rgb(30, 30, 37) };
                    let stroke = if selected { Stroke::new(1.5, ACCENT) } else { Stroke::NONE };
                    let response = ui.add(
                        egui::Button::new(
                            RichText::new(format!("{title}\n{desc}"))
                                .size(12.0)
                                .color(if selected { Color32::WHITE } else { Color32::from_rgb(190, 190, 200) }),
                        )
                        .fill(fill)
                        .stroke(stroke)
                        .min_size(egui::vec2(174.0, 62.0)),
                    );
                    if response.clicked() {
                        self.send(Command::SetOperatingMode { mode: key.to_string() });
                        self.notify(format!("Modalità: {title}"));
                    }
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Windows e Prestazioni usano il profilo ACPI bilanciato; Turbo usa performance. Manuale applica curve e limiti salvati per AC/batteria.",
                )
                .size(11.0)
                .weak(),
            );
        });

        ui.add_space(14.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("CPU").size(16.0).strong());
                    ui.label(RichText::new(&state.system.model).size(11.0).weak());
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        stat(ui, "Frequenza", &format!("{:.2} GHz", state.cpu.freq_mhz / 1000.0));
                        ui.add_space(18.0);
                        stat(ui, "Utilizzo", &format!("{:.0} %", state.cpu.usage_pct));
                        ui.add_space(18.0);
                        stat(
                            ui,
                            "Temperatura",
                            &self
                                .cpu_temp()
                                .map(|t| format!("{t:.0} °C"))
                                .unwrap_or_else(|| "—".into()),
                        );
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "Governor: {}   ·   Load: {:.2}",
                            state.system.cpu_governor, state.system.load1
                        ))
                        .size(11.0)
                        .weak(),
                    );
                });
            });

            card(&mut cols[1], |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("GPU").size(16.0).strong());
                    if let Some(n) = &state.nvidia {
                        ui.label(RichText::new(&n.name).size(11.0).weak());
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            stat(ui, "Frequenza", &format!("{:.0} MHz", n.clock_mhz));
                            ui.add_space(14.0);
                            stat(ui, "Utilizzo", &format!("{:.0} %", n.util_pct));
                            ui.add_space(14.0);
                            stat(ui, "Temperatura", &format!("{:.0} °C", n.temp_c));
                        });
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!(
                                "Core {:.0} MHz · Mem {:.0} MHz · Potenza {:.0} W / max {:.0} W · VRAM {:.0}/{:.0} MB",
                                n.clock_mhz,
                                n.mem_clock_mhz,
                                n.power_w,
                                n.max_power_limit_w,
                                n.mem_used_mb,
                                n.mem_total_mb
                            ))
                            .size(11.0)
                            .weak(),
                        );
                    } else {
                        ui.label(RichText::new("dGPU non disponibile o disattivata.").weak().size(12.0));
                    }
                });
            });
        });

        ui.add_space(14.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Ventole").size(16.0).strong());
                    ui.add_space(8.0);
                    for fan in &state.fans {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}:", fan.label)).size(12.0));
                            ui.label(RichText::new(format!("{} RPM", fan.rpm)).size(14.0).strong().color(ACCENT));
                            ui.label(
                                RichText::new(if fan.custom { "custom" } else { "auto BIOS" })
                                    .size(11.0)
                                    .weak(),
                            );
                        });
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Pressione acustica: non esposta dall'EC su Linux.")
                            .size(10.0)
                            .weak(),
                    );
                });
            });

            card(&mut cols[1], |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Memoria e archiviazione").size(16.0).strong());
                    ui.add_space(6.0);
                    let mem_pct = if state.memory.total_mb > 0 {
                        state.memory.used_mb as f32 / state.memory.total_mb as f32 * 100.0
                    } else {
                        0.0
                    };
                    ui.label(
                        RichText::new(format!(
                            "RAM: {:.1} / {:.1} GB ({:.0} %)",
                            state.memory.used_mb as f32 / 1024.0,
                            state.memory.total_mb as f32 / 1024.0,
                            mem_pct
                        ))
                        .size(12.0),
                    );
                    ui.add(
                        egui::ProgressBar::new(mem_pct / 100.0)
                            .desired_width(ui.available_width())
                            .text(format!("{mem_pct:.0} %")),
                    );
                    ui.add_space(8.0);
                    let disk_pct = if state.storage.total_gb > 0.0 {
                        state.storage.used_gb / state.storage.total_gb * 100.0
                    } else {
                        0.0
                    };
                    ui.label(
                        RichText::new(format!(
                            "Disco: {:.1} / {:.1} GB ({:.0} %)",
                            state.storage.used_gb, state.storage.total_gb, disk_pct
                        ))
                        .size(12.0),
                    );
                    ui.add(
                        egui::ProgressBar::new(disk_pct / 100.0)
                            .desired_width(ui.available_width())
                            .text(format!("{disk_pct:.0} %")),
                    );
                });
            });
        });

        ui.add_space(14.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Batteria").size(16.0).strong());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        stat(ui, "Carica", &format!("{} %", state.battery.capacity));
                        ui.add_space(20.0);
                        stat(ui, "Stato", &state.battery.status);
                        ui.add_space(20.0);
                        stat(ui, "Consumo", &format!("{:.1} W", state.battery.power_now_w));
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if state.battery.ac_online {
                            "Alimentazione da rete"
                        } else {
                            "Alimentazione a batteria"
                        })
                        .size(12.0)
                        .color(if state.battery.ac_online { GREEN } else { YELLOW }),
                    );
                });
            });

            card(&mut cols[1], |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Piattaforma").size(16.0).strong());
                    ui.add_space(6.0);
                    row(ui, "Modello", &state.system.model);
                    row(ui, "Kernel", &state.system.kernel);
                    row(ui, "Sistema", &state.system.os);
                });
            });
        });
    }

    fn keyboard_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Tastiera", "Retroilluminazione e illuminazione Aura RGB");
        ui.add_space(12.0);

        card(ui, |ui| {
            ui.label(RichText::new("Retroilluminazione").size(16.0).strong());
            ui.label(RichText::new("Luminosità del LED tastiera gestita dal firmware ASUS.").weak().size(12.0));
            ui.add_space(10.0);
            let max = state.keyboard.max_brightness.max(1);
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::Slider::new(&mut self.kbd_brightness, 0..=max)
                        .show_value(false)
                        .trailing_fill(true),
                );
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                    self.send(Command::SetKbdBrightness { value: self.kbd_brightness });
                }
                ui.label(
                    RichText::new(format!("{} / {}", self.kbd_brightness, max))
                        .size(18.0)
                        .strong()
                        .color(ACCENT),
                );
            });
            let mut clicked: Option<u8> = None;
            ui.horizontal(|ui| {
                for v in 0..=max {
                    let name = match v {
                        0 => "Spenta",
                        1 => "Bassa",
                        2 => "Media",
                        _ => "Alta",
                    };
                    if ui
                        .add(
                            egui::Button::new(name)
                                .selected(self.kbd_brightness == v)
                                .min_size(egui::vec2(90.0, 30.0)),
                        )
                        .clicked()
                    {
                        clicked = Some(v);
                    }
                }
            });
            if let Some(v) = clicked {
                self.kbd_brightness = v;
                self.send(Command::SetKbdBrightness { value: v });
            }
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Aura RGB").size(16.0).strong());
                if !state.keyboard.has_rgb {
                    ui.label(RichText::new("controller RGB non rilevato").color(RED).size(12.0));
                }
            });
            ui.label(
                RichText::new(
                    "Effetti gestiti direttamente dal controller USB della tastiera. Questo modello supporta un solo effetto globale.",
                )
                .weak()
                .size(12.0),
            );
            ui.label(
                RichText::new(
                    "Aura Sync, effetti AI/Musica e Aura Creator richiedono Armoury Crate: qui sono disponibili tutti gli effetti hardware.",
                )
                .size(11.0)
                .weak(),
            );
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(420.0);
                    egui::Grid::new("aura_grid")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Effetto");
                            egui::ComboBox::from_id_salt("aura_mode")
                                .selected_text(aura_mode_name(self.aura_mode))
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    for (name, value) in AURA_MODES {
                                        ui.selectable_value(&mut self.aura_mode, *value, *name);
                                    }
                                });
                            ui.end_row();

                            ui.label("Velocità");
                            egui::ComboBox::from_id_salt("aura_speed")
                                .selected_text(speed_name(self.aura_speed))
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.aura_speed, 0, "Bassa");
                                    ui.selectable_value(&mut self.aura_speed, 1, "Media");
                                    ui.selectable_value(&mut self.aura_speed, 2, "Alta");
                                });
                            ui.end_row();

                            ui.label("Direzione");
                            egui::ComboBox::from_id_salt("aura_dir")
                                .selected_text(direction_name(self.aura_dir))
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    for (i, name) in ["Destra", "Sinistra", "Su", "Giù"].iter().enumerate() {
                                        ui.selectable_value(&mut self.aura_dir, i as u8, *name);
                                    }
                                });
                            ui.end_row();

                            ui.label("Colore primario");
                            ui.horizontal(|ui| {
                                ui.color_edit_button_srgb(&mut self.aura_c1);
                                ui.label(RichText::new(hex(self.aura_c1)).monospace().size(11.0));
                            });
                            ui.end_row();

                            ui.label("Colore secondario");
                            ui.horizontal(|ui| {
                                ui.color_edit_button_srgb(&mut self.aura_c2);
                                ui.label(RichText::new(hex(self.aura_c2)).monospace().size(11.0));
                            });
                            ui.end_row();
                        });
                });

                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Anteprima").size(12.0).weak());
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(180.0, 70.0), egui::Sense::hover());
                    let c = Color32::from_rgb(self.aura_c1[0], self.aura_c1[1], self.aura_c1[2]);
                    let c2 = Color32::from_rgb(self.aura_c2[0], self.aura_c2[1], self.aura_c2[2]);
                    let painter = ui.painter();
                    painter.rect_filled(rect, CornerRadius::same(8), Color32::from_rgb(10, 10, 13));
                    let left = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(rect.width() / 2.0 - 10.0, rect.height() - 16.0));
                    let right = egui::Rect::from_min_size(egui::pos2(rect.center().x + 2.0, rect.min.y + 8.0), egui::vec2(rect.width() / 2.0 - 10.0, rect.height() - 16.0));
                    painter.rect_filled(left, CornerRadius::same(6), c);
                    painter.rect_filled(right, CornerRadius::same(6), c2);
                    ui.add_space(12.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Applica effetto").strong())
                                .fill(ACCENT)
                                .min_size(egui::vec2(180.0, 34.0)),
                        )
                        .clicked()
                    {
                        let mode = self.aura_mode;
                        let c1 = self.aura_c1;
                        let c2 = self.aura_c2;
                        let speed = self.aura_speed;
                        let dir = self.aura_dir;
                        self.send(Command::SetAura { mode, colour1: c1, colour2: c2, speed, direction: dir });
                        self.notify("Effetto Aura applicato");
                    }
                    if ui
                        .add(egui::Button::new("Spegni illuminazione").min_size(egui::vec2(180.0, 30.0)))
                        .clicked()
                    {
                        self.kbd_brightness = 0;
                        self.send(Command::SetKbdBrightness { value: 0 });
                    }
                    if state.keyboard.brightness == 0 {
                        ui.label(RichText::new("Il LED è spento: riaccendilo dal cursore qui sopra.").size(11.0).color(YELLOW));
                    }
                });
            });
        });
    }

    fn fans_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Ventole", "Modalità automatica BIOS o curva personalizzata a 8 punti");
        ui.add_space(12.0);
        ui.label(
            RichText::new("Le curve personalizzate vengono scritte nel driver asus_custom_fan_curve. Temperature e percentuali devono essere crescenti.")
                .weak()
                .size(12.0),
        );
        ui.add_space(12.0);

        for idx in 0..state.fans.len().min(2) {
            let fan = &state.fans[idx];
            let current = if fan.fan == 1 { self.cpu_temp() } else { self.gpu_temp() };
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} Fan", fan.label)).size(17.0).strong());
                    ui.label(RichText::new(format!("{} RPM", fan.rpm)).size(13.0).color(ACCENT));
                    if let Some(t) = current {
                        ui.label(RichText::new(format!("{t:.0} °C")).size(13.0).color(temp_color(t)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(if fan.custom { "curva personalizzata" } else { "automatico BIOS" })
                                .size(11.0)
                                .color(if fan.custom { YELLOW } else { GREEN }),
                        );
                    });
                });
                ui.add_space(8.0);

                fan_graph(ui, &self.fans[idx], current);
                if self.fans[idx].validate().is_err() {
                    ui.label(
                        RichText::new(
                            "La curva letta dal BIOS non è monotona: sistema temperature e percentuali prima di applicare una curva personalizzata.",
                        )
                        .size(11.0)
                        .color(YELLOW),
                    );
                }
                ui.add_space(10.0);

                egui::Grid::new(format!("fan_points_{idx}"))
                    .num_columns(8)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for p in 0..8 {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(format!("#{}", p + 1)).size(10.0).weak());
                                ui.add(
                                    egui::DragValue::new(&mut self.fans[idx].temps[p])
                                        .range(20..=110)
                                        .speed(0.5)
                                        .suffix(" °C"),
                                );
                                ui.add(
                                    egui::DragValue::new(&mut self.fans[idx].pct[p])
                                        .range(0..=100)
                                        .speed(0.5)
                                        .suffix(" %"),
                                );
                            });
                        }
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("Applica curva").strong()).fill(ACCENT))
                        .clicked()
                    {
                        match self.fans[idx].validate() {
                            Ok(()) => {
                                let points = self.fans[idx].points();
                                self.send(Command::SetFanCurve { fan: fan.fan, points, enabled: true });
                                self.notify(format!("Curva {} applicata", fan.label));
                            }
                            Err(e) => {
                                self.toast = Some(Toast { text: e, error: true, at: Instant::now() });
                            }
                        }
                    }
                    if ui.button("Torna automatico").clicked() {
                        let points = self.fans[idx].points();
                        self.send(Command::SetFanCurve { fan: fan.fan, points, enabled: false });
                        self.notify(format!("{} in modalità automatica", fan.label));
                    }
                    if ui.button("Ricarica dall'hardware").clicked() {
                        self.drafts_ready = false;
                        self.refresh();
                    }
                });
            });
            ui.add_space(14.0);
        }
    }

    fn power_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Alimentazione", "Profili, limiti CPU/GPU e modalità manuale");
        ui.add_space(12.0);

        card(ui, |ui| {
            ui.label(RichText::new("Profilo ACPI").size(16.0).strong());
            ui.label(
                RichText::new("Controllo diretto del firmware (quiet / balanced / performance).")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for choice in &state.profile.choices {
                    let selected = profile_key(&state.profile.current) == profile_key(choice);
                    let (title, desc) = profile_text(choice);
                    let fill = if selected { ACCENT_SOFT } else { Color32::from_rgb(30, 30, 37) };
                    let stroke = if selected { Stroke::new(1.5, ACCENT) } else { Stroke::NONE };
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("{title}\n{desc}"))
                                    .size(12.0)
                                    .color(if selected { Color32::WHITE } else { Color32::from_rgb(190, 190, 200) }),
                            )
                            .fill(fill)
                            .stroke(stroke)
                            .min_size(egui::vec2(180.0, 62.0)),
                        )
                        .clicked()
                    {
                        self.send(Command::SetProfile { profile: choice.clone() });
                        self.notify(format!("Profilo impostato: {title}"));
                    }
                }
            });
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Limiti CPU (PPT)").size(16.0).strong());
                if state.ppt.auto {
                    ui.label(RichText::new("auto firmware").size(11.0).color(YELLOW));
                }
            });
            ui.label(
                RichText::new("SPL = limite sostenuto, SPPT = limite di breve durata. 5 W significa auto/default.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("SPL");
                ui.add(egui::Slider::new(&mut self.ppt_spl, 5..=90).suffix(" W"));
                ui.add_space(16.0);
                ui.label("SPPT");
                ui.add(egui::Slider::new(&mut self.ppt_sppt, 5..=90).suffix(" W"));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(RichText::new("Applica limiti").strong()).fill(ACCENT))
                    .clicked()
                {
                    self.send(Command::SetPpt { spl: self.ppt_spl, sppt: self.ppt_sppt });
                    self.notify("Limiti CPU applicati");
                }
                if ui.button("Ripristina auto").clicked() {
                    self.ppt_spl = 5;
                    self.ppt_sppt = 5;
                    self.send(Command::SetPpt { spl: 5, sppt: 5 });
                    self.notify("Limiti CPU in auto");
                }
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("FPPT e APU SPPT non sono esposti dal kernel su questo modello.")
                    .size(11.0)
                    .weak(),
            );
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.label(RichText::new("Limiti GPU NVIDIA").size(16.0).strong());
            ui.label(
                RichText::new("Thermal target e Dynamic Boost (WMI asus-nb-wmi).")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Thermal target");
                ui.add(egui::Slider::new(&mut self.nv_temp, 60..=90).suffix(" °C"));
                ui.add_space(16.0);
                ui.label("Dynamic Boost");
                ui.add(egui::Slider::new(&mut self.nv_boost, 0..=25).suffix(" W"));
            });
            ui.add_space(8.0);
            if ui
                .add(egui::Button::new(RichText::new("Applica limiti GPU").strong()).fill(ACCENT))
                .clicked()
            {
                self.send(Command::SetNvLimits {
                    temp_target: self.nv_temp,
                    dynamic_boost: self.nv_boost,
                });
                self.notify("Limiti GPU applicati");
            }
            ui.label(
                RichText::new(format!(
                    "Valori attuali: {} °C, {} W.",
                    state.gpu.nv_temp_target, state.gpu.nv_dynamic_boost
                ))
                .size(11.0)
                .weak(),
            );
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.label(RichText::new("CPU").size(16.0).strong());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Governor:");
                ui.label(RichText::new(&state.system.cpu_governor).monospace().color(ACCENT));
            });
            if let Some(mut boost) = state.system.cpu_boost {
                ui.add_space(4.0);
                if ui.checkbox(&mut boost, "CPU Boost (amd-pstate)").changed() {
                    self.send(Command::SetCpuBoost { value: boost });
                    self.notify(if boost { "CPU Boost attivato" } else { "CPU Boost disattivato" });
                }
            }
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Modalità manuale").size(16.0).strong());
                let mut enabled = state.manual.enabled;
                if ui.checkbox(&mut enabled, "attiva").changed() {
                    self.send(Command::SetManualEnabled { enabled });
                    self.notify(if enabled {
                        "Modalità manuale attiva"
                    } else {
                        "Modalità manuale disattivata (ventole auto)"
                    });
                }
            });
            ui.label(
                RichText::new(
                    "Salva curve ventole, PPT e limiti NVIDIA per AC e batteria. Il demone li applica automaticamente al cambio di alimentazione.",
                )
                .weak()
                .size(12.0),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Salva impostazioni correnti per AC").clicked() {
                    self.save_manual("ac");
                }
                if ui.button("Salva impostazioni correnti per batteria").clicked() {
                    self.save_manual("battery");
                }
                if ui.button("Applica profilo ora").clicked() {
                    self.send(Command::SetManualEnabled { enabled: true });
                    self.notify("Profilo manuale applicato");
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "Sorgente attuale: {}. Profilo AC: {} | Profilo batteria: {}",
                    if state.manual.source == "ac" { "rete" } else { "batteria" },
                    manual_summary(&state.manual.ac),
                    manual_summary(&state.manual.battery)
                ))
                .size(11.0)
                .weak(),
            );
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
        });
        self.notify(if source == "ac" {
            "Profilo manuale AC salvato"
        } else {
            "Profilo manuale batteria salvato"
        });
    }

    fn battery_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Batteria", "Stato, consumo e limite di ricarica");
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            card(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(320.0);
                    ui.label(RichText::new("Carica").size(12.0).weak());
                    ui.label(RichText::new(format!("{}%", state.battery.capacity)).size(46.0).strong().color(ACCENT));
                    ui.label(RichText::new(&state.battery.status).size(13.0).weak());
                });
            });
            ui.add_space(10.0);
            card(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(320.0);
                    ui.label(RichText::new("Consumo attuale").size(12.0).weak());
                    ui.label(RichText::new(format!("{:.1} W", state.battery.power_now_w)).size(30.0).strong());
                    ui.label(RichText::new("Limite ricarica").size(12.0).weak());
                    ui.label(RichText::new(format!("{}%", state.battery.charge_limit)).size(22.0).color(YELLOW));
                });
            });
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.label(RichText::new("Limite di ricarica").size(16.0).strong());
            ui.label(
                RichText::new("Ridurre il limite aumenta la vita della batteria. 100% = ricarica completa.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(10.0);

            let mut limited = self.charge_limit < 100;
            if ui.checkbox(&mut limited, "Limita la ricarica").changed() {
                self.charge_limit = if limited { 80 } else { 100 };
                self.send(Command::SetChargeLimit { value: self.charge_limit });
            }
            let resp = ui.add_enabled(
                limited,
                egui::Slider::new(&mut self.charge_limit, 40..=100)
                    .suffix(" %")
                    .step_by(5.0),
            );
            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                self.send(Command::SetChargeLimit { value: self.charge_limit });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for v in [60u8, 80, 90, 100] {
                    if ui
                        .add(
                            egui::Button::new(format!("{v}%"))
                                .selected(state.battery.charge_limit == v)
                                .min_size(egui::vec2(64.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.charge_limit = v;
                        self.send(Command::SetChargeLimit { value: v });
                    }
                }
            });
        });
    }

    fn gpu_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "GPU e display", "Modalità grafica, luminosità pannello e statistiche NVIDIA");
        ui.add_space(12.0);

        if state.gpu.pending_reboot {
            Frame::default()
                .fill(Color32::from_rgb(58, 48, 18))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.label(RichText::new("Riavvio richiesto per applicare le modifiche GPU/BIOS.").strong().color(YELLOW));
                });
            ui.add_space(10.0);
        }

        card(ui, |ui| {
            ui.label(RichText::new("Modalità GPU").size(16.0).strong());
            ui.label(
                RichText::new("Come in Armoury Crate. Ultimate/Eco richiedono un riavvio.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for (key, title, desc) in GPU_MODES {
                    let selected = state.gpu.mode == *key;
                    let fill = if selected { ACCENT_SOFT } else { Color32::from_rgb(30, 30, 37) };
                    let stroke = if selected { Stroke::new(1.5, ACCENT) } else { Stroke::NONE };
                    let response = ui.add(
                        egui::Button::new(
                            RichText::new(format!("{title}\n{desc}"))
                                .size(12.0)
                                .color(if selected { Color32::WHITE } else { Color32::from_rgb(190, 190, 200) }),
                        )
                        .fill(fill)
                        .stroke(stroke)
                        .min_size(egui::vec2(196.0, 72.0)),
                    );
                    if response.clicked() && !selected {
                        self.confirm = Some(ConfirmAction::GpuMode(key.to_string()));
                    }
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "Mux attuale: {} · dGPU {}",
                    if state.gpu.mux_mode == 1 { "Ultimate" } else { "Hybrid" },
                    if state.gpu.dgpu_disabled { "disattivata" } else { "attiva" }
                ))
                .size(11.0)
                .weak(),
            );
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.label(RichText::new("Display").size(16.0).strong());
            if let Some(bl) = &state.backlight {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Luminosità");
                    let resp = ui.add(
                        egui::Slider::new(&mut self.display_brightness, 0..=bl.max_brightness)
                            .show_value(true),
                    );
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        self.send(Command::SetDisplayBrightness { value: self.display_brightness });
                    }
                });
            } else {
                ui.label(RichText::new("Nessun backlight controllabile rilevato.").weak());
            }
            if let Some(info) = &self.display {
                ui.label(
                    RichText::new(format!(
                        "{} · {} Hz attuali · range {:.0}–{:.0} Hz",
                        info.output, info.current, info.min, info.max
                    ))
                    .size(11.0)
                    .weak(),
                );
            }
        });

        if let Some(n) = &state.nvidia {
            ui.add_space(14.0);
            card(ui, |ui| {
                ui.label(RichText::new("NVIDIA").size(16.0).strong());
                ui.label(RichText::new(&n.name).weak().size(12.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    stat(ui, "Temperatura", &format!("{:.0} °C", n.temp_c));
                    ui.add_space(18.0);
                    stat(ui, "Utilizzo", &format!("{:.0} %", n.util_pct));
                    ui.add_space(18.0);
                    stat(ui, "Potenza", &format!("{:.1} W", n.power_w));
                    ui.add_space(18.0);
                    stat(ui, "TGP max", &format!("{:.0} W", n.max_power_limit_w));
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    stat(ui, "Clock core", &format!("{:.0} MHz", n.clock_mhz));
                    ui.add_space(18.0);
                    stat(ui, "Clock memoria", &format!("{:.0} MHz", n.mem_clock_mhz));
                    ui.add_space(18.0);
                    stat(
                        ui,
                        "VRAM",
                        &format!("{:.1}/{:.1} GB", n.mem_used_mb / 1024.0, n.mem_total_mb / 1024.0),
                    );
                });
            });
        }
    }

    fn system_page(&mut self, ui: &mut egui::Ui, state: &State) {
        page_header(ui, "Sistema", "Configurazione, informazioni, sensori e stato del demone");
        ui.add_space(12.0);

        card(ui, |ui| {
            ui.label(RichText::new("Configurazione di sistema").size(16.0).strong());
            ui.label(
                RichText::new("Impostazioni hardware e della sessione desktop.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(10.0);

            let mut sound = state.gpu.boot_sound;
            if ui.checkbox(&mut sound, "Suono di avvio ASUS").changed() {
                self.send(Command::SetBootSound { value: sound });
            }
            let mut od = state.gpu.panel_overdrive;
            if ui.checkbox(&mut od, "Panel Overdrive (riduce il tempo di risposta)").changed() {
                self.send(Command::SetPanelOverdrive { value: od });
            }

            ui.add_space(8.0);
            if ui.checkbox(&mut self.touchpad_enabled, "Touchpad abilitato").changed() {
                let desired = self.touchpad_enabled;
                if let Err(e) = set_touchpad(desired) {
                    self.touchpad_enabled = !desired;
                    self.toast = Some(Toast { text: e, error: true, at: Instant::now() });
                } else {
                    self.notify(if desired { "Touchpad abilitato" } else { "Touchpad disabilitato" });
                }
                store_session(&SessionConfig {
                    screen_saver: self.screen_saver,
                    touchpad_enabled: self.touchpad_enabled,
                    win_key_enabled: self.win_key_enabled,
                });
            }

            if ui
                .checkbox(
                    &mut self.screen_saver,
                    "Risparmio schermo: 60 Hz a batteria, refresh massimo con l'alimentatore",
                )
                .changed()
            {
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
                ui.label(
                    RichText::new(format!(
                        "{}: {} Hz (range {:.0}–{:.0} Hz, sorgente {})",
                        info.output,
                        info.current,
                        info.min,
                        info.max,
                        if state.battery.ac_online { "rete" } else { "batteria" }
                    ))
                    .size(11.0)
                    .weak(),
                );
            }

            ui.add_space(8.0);
            if ui.checkbox(&mut self.win_key_enabled, "Tasto Windows abilitato (solo X11)").changed() {
                let desired = self.win_key_enabled;
                if let Err(e) = set_win_key(desired) {
                    self.win_key_enabled = !desired;
                    self.toast = Some(Toast { text: e, error: true, at: Instant::now() });
                } else {
                    self.notify(if desired { "Tasto Windows abilitato" } else { "Tasto Windows disabilitato" });
                }
                store_session(&SessionConfig {
                    screen_saver: self.screen_saver,
                    touchpad_enabled: self.touchpad_enabled,
                    win_key_enabled: self.win_key_enabled,
                });
            }
        });

        ui.add_space(14.0);
        card(ui, |ui| {
            ui.label(RichText::new("Piattaforma").size(16.0).strong());
            ui.add_space(8.0);
            egui::Grid::new("sys_grid")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    row(ui, "Modello", &state.system.model);
                    row(ui, "Scheda madre", &state.system.board);
                    row(ui, "Kernel", &state.system.kernel);
                    row(ui, "Distribuzione", &state.system.os);
                    row(ui, "Hostname", &state.system.hostname);
                    row(ui, "Uptime", &format_uptime(state.system.uptime_secs));
                    row(ui, "Load (1m)", &format!("{:.2}", state.system.load1));
                    row(ui, "Governor CPU", &state.system.cpu_governor);
                    row(ui, "Versione demone", &state.system.daemon_version);
                    row(ui, "Socket", &state.system.socket);
                });
        });

        ui.add_space(14.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.vertical(|ui| {
                    ui.set_width(420.0);
                    ui.label(RichText::new("Temperature").size(16.0).strong());
                    ui.add_space(6.0);
                    egui::Grid::new("temps_grid")
                        .num_columns(2)
                        .spacing([24.0, 4.0])
                        .show(ui, |ui| {
                            for t in &state.temps {
                                ui.label(RichText::new(&t.label).size(12.0));
                                ui.label(
                                    RichText::new(format!("{:.1} °C", t.celsius))
                                        .size(12.0)
                                        .color(temp_color(t.celsius)),
                                );
                                ui.end_row();
                            }
                        });
                });
            });
            card(&mut cols[1], |ui| {
                ui.vertical(|ui| {
                    ui.set_width(360.0);
                    ui.label(RichText::new("Ventole").size(16.0).strong());
                    ui.add_space(6.0);
                    for fan in &state.fans {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{}:", fan.label)).size(12.0));
                            ui.label(RichText::new(format!("{} RPM", fan.rpm)).size(12.0).color(ACCENT));
                            ui.label(
                                RichText::new(if fan.custom { "custom" } else { "auto" })
                                    .size(11.0)
                                    .weak(),
                            );
                        });
                    }
                    ui.add_space(10.0);
                    ui.label(RichText::new("Demone").size(16.0).strong());
                    ui.label(
                        RichText::new(if self.error.is_none() && self.state.is_some() {
                            "Connesso e operativo"
                        } else {
                            "Non connesso"
                        })
                        .size(12.0)
                        .color(if self.error.is_none() { GREEN } else { RED }),
                    );
                    ui.add_space(6.0);
                    if ui.button("Riavvia demone").clicked() {
                        let _ = std::process::Command::new("pkexec")
                            .args(["systemctl", "restart", "rog-control-daemon"])
                            .spawn();
                        self.notify("Riavvio del demone richiesto a polkit");
                    }
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

        egui::CentralPanel::default().show(ui, |ui| {
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
    // Font "black" dedicato al wordmark ROG (SIL OFL, incluso in assets/).
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
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = PANEL;
        style.visuals.extreme_bg_color = Color32::from_rgb(10, 10, 13);
        style.visuals.faint_bg_color = PANEL;
        style.visuals.selection.bg_fill = ACCENT_SOFT;
        style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        style.visuals.widgets.active.bg_fill = ACCENT;
        style.visuals.widgets.hovered.bg_fill = ACCENT_SOFT;
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(32, 32, 39);
        style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(32, 32, 39);
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(24.0));
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
    });
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::group(ui.style())
        .fill(PANEL)
        .inner_margin(Margin::same(14))
        .show(ui, add)
        .inner
}

fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(28.0).strong());
    ui.label(RichText::new(subtitle).size(13.0).weak());
}

fn row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).size(12.0).weak());
    ui.label(RichText::new(value).size(12.0).monospace());
    ui.end_row();
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(10.0).weak());
        ui.label(RichText::new(value).size(20.0).strong());
    });
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

fn profile_key(s: &str) -> String {
    s.trim().to_lowercase()
}

fn profile_text(choice: &str) -> (&'static str, &'static str) {
    match profile_key(choice).as_str() {
        "quiet" => ("Silenzioso", "ventole più lente, limiti ridotti"),
        "balanced" => ("Bilanciato", "compromesso potenza/rumore"),
        "performance" => ("Prestazioni", "massima potenza e raffreddamento"),
        "low-power" => ("Risparmio", "consumi minimi"),
        _ => ("Personalizzato", "profilo firmware"),
    }
}

fn fan_graph(ui: &mut egui::Ui, draft: &FanDraft, current: Option<f32>) {
    let height = 150.0;
    let (response, painter) =
        ui.allocate_painter(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let rect = response.rect.shrink2(egui::vec2(34.0, 14.0));
    painter.rect_filled(response.rect, CornerRadius::same(6), Color32::from_rgb(11, 11, 14));

    let t_min = 20.0f32;
    let t_max = 110.0f32;
    let x_of = |t: f32| rect.left() + ((t - t_min) / (t_max - t_min)).clamp(0.0, 1.0) * rect.width();
    let y_of = |p: f32| rect.bottom() - (p / 100.0).clamp(0.0, 1.0) * rect.height();

    let grid = Color32::from_rgb(38, 38, 46);
    for t in [20.0f32, 40.0, 60.0, 80.0, 100.0] {
        let x = x_of(t);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0, grid),
        );
        painter.text(
            egui::pos2(x, rect.bottom() + 2.0),
            Align2::CENTER_TOP,
            format!("{t:.0}°"),
            FontId::proportional(9.0),
            Color32::from_rgb(130, 130, 145),
        );
    }
    for p in [0.0f32, 25.0, 50.0, 75.0, 100.0] {
        let y = y_of(p);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(1.0, grid),
        );
        painter.text(
            egui::pos2(rect.left() - 4.0, y),
            Align2::RIGHT_CENTER,
            format!("{p:.0}%"),
            FontId::proportional(9.0),
            Color32::from_rgb(130, 130, 145),
        );
    }

    let points: Vec<egui::Pos2> = (0..8)
        .map(|i| egui::pos2(x_of(draft.temps[i] as f32), y_of(draft.pct[i] as f32)))
        .collect();
    painter.add(egui::Shape::line(points.clone(), Stroke::new(2.0, ACCENT)));
    for p in &points {
        painter.circle_filled(*p, 3.0, ACCENT);
    }

    if let Some(t) = current {
        let x = x_of(t);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.5, YELLOW),
        );
        painter.text(
            egui::pos2(x + 4.0, rect.top()),
            Align2::LEFT_TOP,
            format!("{t:.0}°C"),
            FontId::proportional(10.0),
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

const GPU_MODES: &[(&str, &str, &str)] = &[
    ("ultimate", "Ultimate", "Solo dGPU, massime prestazioni"),
    ("standard", "Standard", "Hybrid / Optimus"),
    ("eco", "Eco", "dGPU off, massima autonomia"),
    ("optimized", "Ottimizzato", "Hybrid con risparmio automatico"),
];

fn manual_summary(p: &ManualProfile) -> String {
    format!(
        "SPL {} W · SPPT {} W · GPU {} °C / {} W · ventole 8 punti",
        p.spl, p.sppt, p.nv_temp_target, p.nv_dynamic_boost
    )
}

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
