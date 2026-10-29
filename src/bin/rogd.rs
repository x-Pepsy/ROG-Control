use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{chown, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use rog_control::hardware::{Hardware, HwResult};
use rog_control::protocol::*;

fn group_gid(name: &str) -> Option<u32> {
    let text = fs::read_to_string("/etc/group").ok()?;
    for line in text.lines() {
        let mut parts = line.split(':');
        if parts.next() == Some(name) {
            parts.next();
            return parts.next()?.parse().ok();
        }
    }
    None
}

fn prepare_socket(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    }
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    let listener = UnixListener::bind(path)?;

    // Accessibile al gruppo plugdev (l'utente desktop è membro).
    let mut mode = 0o666;
    if let Some(gid) = group_gid("plugdev") {
        if chown(path, Some(0), Some(gid)).is_ok() {
            mode = 0o660;
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    if mode == 0o666 {
        eprintln!("avviso: socket accessibile a tutti (gruppo plugdev non disponibile)");
    }
    Ok(listener)
}

fn handle_command(hw: &mut Hardware, command: Command) -> HwResult<()> {
    match command {
        Command::State => Ok(()),
        Command::SetKbdBrightness { value } => hw.set_kbd_brightness(value),
        Command::SetAura { mode, colour1, colour2, speed, direction } => {
            hw.set_aura(mode, colour1, colour2, speed, direction)
        }
        Command::SetDisplayBrightness { value } => hw.set_display_brightness(value),
        Command::SetFanCurve { fan, points, enabled } => {
            hw.set_fan_curve(fan, &points, enabled)
        }
        Command::SetProfile { profile } => hw.set_profile(&profile),
        Command::SetOperatingMode { mode } => hw.set_operating_mode(&mode),
        Command::SetGpuMode { mode } => hw.set_gpu_mode(&mode),
        Command::SetPpt { spl, sppt, fppt } => hw.set_ppt(spl, sppt, fppt),
        Command::SetNvLimits { temp_target, dynamic_boost } => {
            hw.set_nv_limits(temp_target, dynamic_boost)
        }
        Command::SetGpuOffsets { base_clock, memory_clock } => {
            hw.set_gpu_offsets(base_clock, memory_clock)
        }
        Command::SaveManualProfile {
            source,
            cpu,
            gpu,
            nv_temp_target,
            nv_dynamic_boost,
            spl,
            sppt,
            fppt,
        } => hw.save_manual_profile(
            &source,
            cpu,
            gpu,
            nv_temp_target,
            nv_dynamic_boost,
            spl,
            sppt,
            fppt,
        ),
        Command::SetManualEnabled { enabled } => hw.set_manual_enabled(enabled),
        Command::SetChargeLimit { value } => hw.set_charge_limit(value),
        Command::SetPanelOverdrive { value } => hw.set_panel_overdrive(value),
        Command::SetBootSound { value } => hw.set_boot_sound(value),
        Command::SetDgpuDisabled { value } => hw.set_dgpu_disabled(value),
        Command::SetGpuMux { value } => hw.set_gpu_mux(value),
        Command::SetCpuBoost { value } => hw.set_cpu_boost(value),
    }
}

fn handle_connection(stream: UnixStream, hw: &mut Hardware) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            let resp = Response::err(0, format!("richiesta JSON non valida: {e}"));
            let mut s = stream;
            let _ = writeln!(s, "{}", serde_json::to_string(&resp).unwrap_or_default());
            return;
        }
    };

    let id = request.id;
    let response = match handle_command(hw, request.command) {
        Ok(()) => Response::ok(id, hw.state()),
        Err(e) => Response::err(id, e),
    };

    let mut s = stream;
    match serde_json::to_string(&response) {
        Ok(json) => {
            let _ = writeln!(s, "{json}");
        }
        Err(e) => eprintln!("serializzazione risposta fallita: {e}"),
    }
}

fn ac_online() -> bool {
    fs::read_to_string("/sys/class/power_supply/ADP0/online")
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Applica automaticamente il profilo manuale giusto quando cambia la sorgente
/// di alimentazione (AC <-> batteria), come fa Armoury Crate.
fn spawn_power_monitor() {
    std::thread::spawn(|| {
        let mut hw = Hardware::new();
        if hw.config.manual_enabled {
            if let Err(e) = hw.apply_manual_for_current_source() {
                eprintln!("profilo manuale non applicato all'avvio: {e}");
            }
        }
        let mut last = ac_online();
        loop {
            std::thread::sleep(Duration::from_secs(3));
            let current = ac_online();
            if current != last {
                last = current;
                hw.reload_config();
                if hw.config.manual_enabled {
                    match hw.apply_manual_for_current_source() {
                        Ok(()) => eprintln!(
                            "profilo manuale applicato per {}",
                            if current { "AC" } else { "batteria" }
                        ),
                        Err(e) => eprintln!("cambio sorgente, profilo manuale non applicato: {e}"),
                    }
                }
            }
        }
    });
}

fn main() {
    let path = Path::new(SOCKET_PATH);
    let listener = match prepare_socket(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("impossibile creare il socket {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    eprintln!("rog-control-daemon {} in ascolto su {}", DAEMON_VERSION, SOCKET_PATH);
    let mut hw = Hardware::new();
    spawn_power_monitor();

    // Il controller RGB è un dispositivo USB: al boot può comparire dopo il demone.
    if hw.has_saved_aura() {
        for attempt in 0..10 {
            match hw.reapply_saved_aura() {
                Ok(()) => {
                    eprintln!("effetto Aura ripristinato");
                    break;
                }
                Err(e) => {
                    if attempt == 9 {
                        eprintln!("impossibile ripristinare l'effetto Aura: {e}");
                    } else {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }
    }

    for stream in listener.incoming() {
        match stream {
            Ok(s) => handle_connection(s, &mut hw),
            Err(e) => eprintln!("connessione fallita: {e}"),
        }
    }
}
