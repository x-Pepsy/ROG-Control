//! Profili per modello: i range di PPT, GPU e clock NON sono uguali su tutti
//! i ROG Strix. Il profilo viene scelto dal DMI e può essere sovrascritto con
//! `/etc/rog-control/device.json`.

use std::fs;

use crate::protocol::{DeviceLimits, ValueRange};

const FW_ATTRS: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";
const WMI: &str = "/sys/devices/platform/asus-nb-wmi";
const OVERRIDE: &str = "/etc/rog-control/device.json";

fn read_opt(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn fw_num(attr: &str, field: &str) -> Option<i32> {
    read_opt(&format!("{FW_ATTRS}/{attr}/{field}"))?.parse().ok()
}

fn range_from_fw(attr: &str) -> Option<ValueRange> {
    let min = fw_num(attr, "min_value")?;
    let max = fw_num(attr, "max_value")?;
    if max >= min && min >= 0 {
        Some(ValueRange { min, max })
    } else {
        None
    }
}

fn valid_range(r: ValueRange, max_limit: i32) -> bool {
    r.min >= 0 && r.max >= r.min && r.max <= max_limit
}

fn dmi() -> (String, String) {
    let product = read_opt("/sys/devices/virtual/dmi/id/product_name").unwrap_or_default();
    let board = read_opt("/sys/devices/virtual/dmi/id/board_name").unwrap_or_default();
    (product, board)
}

fn builtin_g713rm() -> DeviceLimits {
    DeviceLimits {
        profile: "ROG Strix G713RM (G713RM)".to_string(),
        source: "builtin".to_string(),
        // Valori ufficiali Armoury Crate per G713RM.
        ppt_spl: ValueRange { min: 28, max: 90 },
        ppt_sppt: ValueRange { min: 28, max: 100 },
        ppt_fppt: ValueRange { min: 28, max: 125 },
        fppt_supported: true,
        gpu_temp: ValueRange { min: 75, max: 87 },
        gpu_boost: ValueRange { min: 0, max: 25 },
        gpu_base: ValueRange { min: 0, max: 200 },
        gpu_mem: ValueRange { min: 0, max: 300 },
    }
}

fn generic(profile: &str) -> DeviceLimits {
    let spl = range_from_fw("ppt_pl1_spl");
    let sppt = range_from_fw("ppt_pl2_sppt");
    let fppt = range_from_fw("ppt_pl3_fppt");
    let fppt_supported = fppt.is_some()
        || std::path::Path::new(&format!("{WMI}/ppt_fppt")).exists();

    DeviceLimits {
        profile: format!("{profile} (profilo generico)"),
        source: "generic".to_string(),
        // Se il firmware espone i range (asus-armoury) li usiamo, altrimenti
        // ripieghiamo su limiti prudenti.
        ppt_spl: spl.unwrap_or(ValueRange { min: 28, max: 90 }),
        ppt_sppt: sppt.unwrap_or(ValueRange { min: 28, max: 90 }),
        ppt_fppt: fppt.unwrap_or(ValueRange { min: 28, max: 90 }),
        fppt_supported,
        gpu_temp: ValueRange { min: 75, max: 87 },
        gpu_boost: ValueRange { min: 0, max: 25 },
        gpu_base: ValueRange { min: 0, max: 200 },
        gpu_mem: ValueRange { min: 0, max: 300 },
    }
}

/// Cerca un override specifico per modello.
///
/// Formato: stessa struttura di `DeviceLimits` (profile/source opzionali).
/// I range vengono validati; se il file è invalido viene ignorato.
fn load_override() -> Option<DeviceLimits> {
    let text = fs::read_to_string(OVERRIDE).ok()?;
    let mut limits: DeviceLimits = serde_json::from_str(&text).ok()?;

    if limits.profile.trim().is_empty() {
        limits.profile = "Custom device profile".to_string();
    }
    let ok = valid_range(limits.ppt_spl, 500)
        && valid_range(limits.ppt_sppt, 500)
        && valid_range(limits.ppt_fppt, 500)
        && valid_range(limits.gpu_temp, 120)
        && valid_range(limits.gpu_boost, 100)
        && valid_range(limits.gpu_base, 1000)
        && valid_range(limits.gpu_mem, 2000);
    if !ok {
        eprintln!("device.json ignorato: range non validi");
        return None;
    }
    limits.source = "config".to_string();
    Some(limits)
}

pub fn detect() -> DeviceLimits {
    if let Some(custom) = load_override() {
        return custom;
    }
    let (product, board) = dmi();
    let joined = format!("{product} {board}").to_uppercase();
    if joined.contains("G713RM") {
        return builtin_g713rm();
    }
    let name = if product.is_empty() { "ASUS ROG".to_string() } else { product };
    generic(&name)
}
