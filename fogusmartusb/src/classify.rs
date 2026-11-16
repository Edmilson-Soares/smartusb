//! Reconhecimento do tipo de um aparelho USB a partir do que dá para saber
//! sem abri-lo: classes das interfaces, coleções HID (usage page/usage),
//! nomes, VID/PID (base embutida, do usuário e usb.ids) e as portas COM e
//! leitores PC/SC associados.

use crate::ids::{self, Hint};

#[derive(Clone, Debug, Default)]
pub struct Iface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct HidCol {
    pub usage_page: u16,
    pub usage: u16,
    pub interface: i32,
    pub path: String,
}

/// O que se sabe de um aparelho antes de classificar.
#[derive(Clone, Debug, Default)]
pub struct Raw {
    pub vid: u16,
    pub pid: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub interfaces: Vec<Iface>,
    pub hid: Vec<HidCol>,
    pub ports: Vec<String>,
    pub readers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Class {
    pub kind: String,
    pub kinds: Vec<String>,
    pub name: String,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub category: &'static str,
    pub confidence: &'static str,
    pub source: &'static str,
    /// Como ler dados do aparelho pela API de scanner: `hidpos`, `keyboard`,
    /// `serial`, `hid`.
    pub modes: Vec<&'static str>,
}

/// Ordem de precedência: o mais específico primeiro.
const PRIORITY: &[&str] = &[
    "barcode_scanner",
    "security_token",
    "fido_key",
    "nfc_reader",
    "smartcard_reader",
    "rfid_reader",
    "fingerprint_reader",
    "msr",
    "scale",
    "printer",
    "camera",
    "phone",
    "storage",
    "audio",
    "network",
    "bluetooth",
    "serial",
    "gamepad",
    "touch",
    "ups",
    "keyboard",
    "mouse",
    "hid",
    "hub",
    "vendor",
    "unknown",
];

pub fn category(kind: &str) -> &'static str {
    match kind {
        "barcode_scanner" => "Leitor de código de barras / QR Code",
        "security_token" => "Token de certificado digital / chave de segurança",
        "fido_key" => "Chave de segurança FIDO/U2F",
        "nfc_reader" => "Leitor NFC / cartão sem contato",
        "smartcard_reader" => "Leitor de smart card",
        "rfid_reader" => "Leitor RFID / crachá de proximidade",
        "fingerprint_reader" => "Leitor biométrico (digital)",
        "msr" => "Leitor de tarja magnética",
        "scale" => "Balança",
        "printer" => "Impressora",
        "camera" => "Câmera / webcam",
        "phone" => "Celular / tablet",
        "storage" => "Armazenamento (pen drive, HD, leitor de cartão SD)",
        "audio" => "Áudio",
        "network" => "Adaptador de rede",
        "bluetooth" => "Adaptador Bluetooth",
        "serial" => "Porta serial (COM)",
        "gamepad" => "Controle de jogo",
        "touch" => "Tela sensível ao toque / mesa digitalizadora",
        "ups" => "No-break (UPS)",
        "keyboard" => "Teclado",
        "mouse" => "Mouse",
        "hid" => "Dispositivo HID",
        "hub" => "Hub USB",
        "vendor" => "Dispositivo específico do fabricante",
        _ => "Desconhecido",
    }
}

/// Nome de classe USB (interface ou aparelho).
pub fn class_name(class: u8) -> &'static str {
    match class {
        0x00 => "per-interface",
        0x01 => "audio",
        0x02 => "cdc",
        0x03 => "hid",
        0x05 => "physical",
        0x06 => "image",
        0x07 => "printer",
        0x08 => "mass_storage",
        0x09 => "hub",
        0x0A => "cdc_data",
        0x0B => "smart_card",
        0x0D => "content_security",
        0x0E => "video",
        0x0F => "personal_healthcare",
        0x10 => "audio_video",
        0x11 => "billboard",
        0x12 => "usb_c_bridge",
        0xDC => "diagnostic",
        0xE0 => "wireless",
        0xEF => "miscellaneous",
        0xFE => "application_specific",
        0xFF => "vendor_specific",
        _ => "unknown",
    }
}

/// Nome de uma coleção HID conhecida.
pub fn hid_usage_name(page: u16, usage: u16) -> &'static str {
    match (page, usage) {
        (0x01, 0x02) => "mouse",
        (0x01, 0x04) => "joystick",
        (0x01, 0x05) => "gamepad",
        (0x01, 0x06) => "keyboard",
        (0x01, 0x07) => "keypad",
        (0x01, 0x80) => "system_control",
        (0x0C, _) => "consumer_control",
        (0x0D, _) => "digitizer",
        (0x84, _) | (0x85, _) => "power_device",
        (0x8C, _) => "barcode_scanner",
        (0x8D, _) => "scale",
        (0x8E, _) => "msr",
        (0xF1D0, _) => "fido",
        (p, _) if p >= 0xFF00 => "vendor",
        _ => "other",
    }
}

const SCANNER_WORDS: &[&str] = &["barcode", "bar code", "scanner", "qr code", "qrcode", "imager", "2d scan", "1d scan", "code reader"];
const RFID_WORDS: &[&str] = &["rfid", "125khz", "125 khz", "proximity", "pcprox", "id card reader", "em4100", "card reader keyboard"];
const NFC_WORDS: &[&str] = &["nfc", "picc", "contactless", "acr122", "acr1252"];
const TOKEN_WORDS: &[&str] = &["token", "etoken", "rutoken", "jacarta", "yubikey", "epass", "safenet", "nitrokey", "security key"];
const FINGER_WORDS: &[&str] = &["fingerprint", "finger print", "biometric", "u.are.u", "hamster", "fingprint"];
const SMARTCARD_WORDS: &[&str] = &["smart card", "smartcard", "ccid", "card reader"];

fn has_any(hay: &str, words: &[&str]) -> bool {
    words.iter().any(|w| hay.contains(w))
}

pub fn classify(raw: &Raw) -> Class {
    let lookup = ids::lookup(raw.vid, raw.pid);
    let hint = ids::hint(raw.vid);

    let mut text = String::new();
    for s in [&raw.product, &raw.manufacturer, &lookup.product].into_iter().flatten() {
        text.push_str(&s.to_lowercase());
        text.push(' ');
    }
    for i in &raw.interfaces {
        if let Some(n) = &i.name {
            text.push_str(&n.to_lowercase());
            text.push(' ');
        }
    }
    for r in &raw.readers {
        text.push_str(&r.to_lowercase());
        text.push(' ');
    }

    let mut kinds: Vec<&'static str> = Vec::new();
    fn add(kinds: &mut Vec<&'static str>, k: &'static str) {
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    let mut source = "class";
    let mut confidence = "high";

    // Coleções HID: a usage page diz o que o aparelho é.
    let mut keyboard = false;
    let mut hidpos = false;
    let mut hid_generic = false;
    for h in &raw.hid {
        match hid_usage_name(h.usage_page, h.usage) {
            "barcode_scanner" => {
                add(&mut kinds, "barcode_scanner");
                hidpos = true;
            }
            "scale" => add(&mut kinds, "scale"),
            "msr" => add(&mut kinds, "msr"),
            "fido" => add(&mut kinds, "fido_key"),
            "keyboard" | "keypad" => {
                add(&mut kinds, "keyboard");
                keyboard = true;
            }
            "mouse" => add(&mut kinds, "mouse"),
            "joystick" | "gamepad" => add(&mut kinds, "gamepad"),
            "digitizer" => add(&mut kinds, "touch"),
            "power_device" => add(&mut kinds, "ups"),
            "vendor" => hid_generic = true,
            _ => {}
        }
    }

    // Classes das interfaces (ou do aparelho, se não for composto).
    let mut classes: Vec<(u8, u8, u8)> = raw.interfaces.iter().map(|i| (i.class, i.subclass, i.protocol)).collect();
    if raw.class != 0 && raw.class != 0xEF {
        classes.push((raw.class, raw.subclass, raw.protocol));
    }
    let mut has_hid_iface = !raw.hid.is_empty();
    let mut vendor_iface = false;
    for &(c, s, p) in &classes {
        match c {
            0x01 => add(&mut kinds, "audio"),
            0x02 => match s {
                0x06 | 0x0D | 0x0E => add(&mut kinds, "network"), // ECM, NCM, MBIM
                _ => add(&mut kinds, "serial"),
            },
            0x03 => {
                has_hid_iface = true;
                // Sem a usage page (HID não enumerado), o protocolo de boot ajuda.
                if s == 1 && p == 1 && raw.hid.is_empty() {
                    add(&mut kinds, "keyboard");
                    keyboard = true;
                }
                if s == 1 && p == 2 && raw.hid.is_empty() {
                    add(&mut kinds, "mouse");
                }
            }
            0x06 => add(&mut kinds, if text.contains("mtp") || text.contains("phone") { "phone" } else { "camera" }),
            0x07 => add(&mut kinds, "printer"),
            0x08 => add(&mut kinds, "storage"),
            0x09 => add(&mut kinds, "hub"),
            0x0B => add(&mut kinds, "smartcard_reader"),
            0x0E => add(&mut kinds, "camera"),
            0xE0 if s == 0x01 && p == 0x01 => add(&mut kinds, "bluetooth"),
            0xE0 if s == 0x01 && p == 0x03 => add(&mut kinds, "network"), // RNDIS
            0xEF if s == 0x04 => add(&mut kinds, "network"), // RNDIS sobre misc
            0xFF => vendor_iface = true,
            _ => {}
        }
    }
    if !raw.ports.is_empty() {
        add(&mut kinds, "serial");
    }
    if !raw.readers.is_empty() {
        add(&mut kinds, "smartcard_reader");
    }

    // Dicas por fabricante, só quando a interface combina.
    let readable_as_scanner = keyboard || hidpos || !raw.ports.is_empty();
    match hint {
        Hint::Scanner if readable_as_scanner => {
            add(&mut kinds, "barcode_scanner");
            if !hidpos {
                source = "vendor";
            }
        }
        Hint::Token if kinds.contains(&"smartcard_reader") || kinds.contains(&"fido_key") || has_hid_iface => {
            add(&mut kinds, "security_token");
            source = "vendor";
        }
        Hint::SmartCard if kinds.contains(&"smartcard_reader") && has_any(&text, NFC_WORDS) => add(&mut kinds, "nfc_reader"),
        Hint::Fingerprint if vendor_iface || kinds.is_empty() || has_hid_iface => {
            add(&mut kinds, "fingerprint_reader");
            source = "vendor";
        }
        Hint::Rfid if keyboard || has_hid_iface => {
            add(&mut kinds, "rfid_reader");
            source = "vendor";
        }
        Hint::Msr if has_hid_iface => {
            add(&mut kinds, "msr");
            source = "vendor";
        }
        Hint::Network if vendor_iface && kinds.is_empty() => {
            add(&mut kinds, "network");
            source = "vendor";
        }
        Hint::Printer if vendor_iface && kinds.is_empty() => {
            add(&mut kinds, "printer");
            source = "vendor";
            confidence = "medium";
        }
        _ => {}
    }

    // Palavras no nome do produto/interfaces.
    if has_any(&text, SCANNER_WORDS) && readable_as_scanner && !kinds.contains(&"barcode_scanner") {
        add(&mut kinds, "barcode_scanner");
        source = "name";
        confidence = "medium";
    }
    if has_any(&text, RFID_WORDS) && (keyboard || has_hid_iface || !raw.ports.is_empty()) {
        add(&mut kinds, "rfid_reader");
        source = "name";
    }
    if has_any(&text, TOKEN_WORDS) && (kinds.contains(&"smartcard_reader") || has_hid_iface) {
        add(&mut kinds, "security_token");
        source = "name";
    }
    if has_any(&text, FINGER_WORDS) {
        add(&mut kinds, "fingerprint_reader");
        source = "name";
    }
    if kinds.contains(&"smartcard_reader") && has_any(&text, NFC_WORDS) {
        add(&mut kinds, "nfc_reader");
    }
    if has_any(&text, SMARTCARD_WORDS) && vendor_iface && !kinds.contains(&"storage") {
        add(&mut kinds, "smartcard_reader");
        source = "name";
        confidence = "medium";
    }

    if kinds.is_empty() {
        if hid_generic || has_hid_iface {
            add(&mut kinds, "hid");
        } else if vendor_iface {
            add(&mut kinds, "vendor");
            confidence = "low";
        } else {
            add(&mut kinds, "unknown");
            confidence = "low";
        }
    }

    // Base do usuário/embutida manda no tipo.
    if let Some(k) = &lookup.kind {
        let k: &'static str = PRIORITY.iter().find(|p| **p == k.as_str()).copied().unwrap_or("vendor");
        kinds.retain(|x| *x != k);
        kinds.insert(0, k);
        source = lookup.kind_source.unwrap_or("builtin");
        confidence = "high";
    } else {
        kinds.sort_by_key(|k| PRIORITY.iter().position(|p| p == k).unwrap_or(usize::MAX));
    }
    let kind = kinds[0];

    let mut modes = Vec::new();
    if hidpos {
        modes.push("hidpos");
    }
    if !raw.ports.is_empty() {
        modes.push("serial");
    }
    if keyboard {
        modes.push("keyboard");
    }
    // Coleções do fabricante num teclado/mouse comum (LEDs, firmware) não
    // são fonte de leituras.
    let plain_input = keyboard || kinds.contains(&"mouse");
    if !hidpos && (kinds.contains(&"msr") || kinds.contains(&"scale") || (hid_generic && !plain_input)) {
        modes.push("hid");
    }

    let product = raw.product.clone().filter(|s| !s.trim().is_empty()).or(lookup.product.clone());
    let vendor = raw.manufacturer.clone().filter(|s| !s.trim().is_empty()).or(lookup.vendor.clone());
    // Nome da base do usuário tem precedência sobre o que o aparelho diz.
    let name = if lookup.name_source == Some("user") {
        lookup.product.clone().unwrap()
    } else {
        match (&vendor, &product) {
            (Some(v), Some(p)) if !p.to_lowercase().contains(&first_word(v)) => format!("{v} {p}"),
            (_, Some(p)) => p.clone(),
            (Some(v), None) => format!("{v} ({})", category(kind)),
            (None, None) => format!("{} {:04x}:{:04x}", category(kind), raw.vid, raw.pid),
        }
    };

    Class {
        kind: kind.to_string(),
        kinds: kinds.iter().map(|s| s.to_string()).collect(),
        name,
        vendor,
        product,
        category: category(kind),
        confidence,
        source,
        modes,
    }
}

fn first_word(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hid(page: u16, usage: u16) -> HidCol {
        HidCol { usage_page: page, usage, interface: 0, path: String::new() }
    }

    fn iface(class: u8, subclass: u8, protocol: u8) -> Iface {
        Iface { number: 0, class, subclass, protocol, name: None }
    }

    #[test]
    fn hid_pos_scanner() {
        let c = classify(&Raw { vid: 0x1234, pid: 1, hid: vec![hid(0x8C, 0x02)], interfaces: vec![iface(3, 0, 0)], ..Default::default() });
        assert_eq!(c.kind, "barcode_scanner");
        assert_eq!(c.modes, vec!["hidpos"]);
    }

    #[test]
    fn keyboard_wedge_scanner_by_vendor_and_name() {
        let c = classify(&Raw { vid: 0x0C2E, pid: 0x0B61, hid: vec![hid(1, 6)], interfaces: vec![iface(3, 1, 1)], ..Default::default() });
        assert_eq!(c.kind, "barcode_scanner");
        assert_eq!(c.modes, vec!["keyboard"]);
        assert!(c.kinds.contains(&"keyboard".to_string()));

        let c = classify(&Raw {
            vid: 0x1111,
            pid: 1,
            product: Some("USB Barcode Scanner".into()),
            hid: vec![hid(1, 6)],
            ..Default::default()
        });
        assert_eq!(c.kind, "barcode_scanner");
        assert_eq!(c.source, "name");

        // Teclado comum continua teclado.
        let c = classify(&Raw { vid: 0x046D, pid: 1, product: Some("USB Keyboard".into()), hid: vec![hid(1, 6), hid(1, 2)], ..Default::default() });
        assert_eq!(c.kind, "keyboard");
        assert_eq!(c.kinds, vec!["keyboard", "mouse"]);
    }

    #[test]
    fn serial_scanner() {
        let c = classify(&Raw { vid: 0x05E0, pid: 0x1701, interfaces: vec![iface(2, 2, 1), iface(0x0A, 0, 0)], ports: vec!["COM7".into()], ..Default::default() });
        assert_eq!(c.kind, "barcode_scanner");
        assert_eq!(c.modes, vec!["serial"]);
    }

    #[test]
    fn smartcard_and_tokens() {
        let c = classify(&Raw { vid: 0x058F, pid: 0x9540, interfaces: vec![iface(0x0B, 0, 0)], ..Default::default() });
        assert_eq!(c.kind, "smartcard_reader");
        let c = classify(&Raw { vid: 0x072F, pid: 0x2200, interfaces: vec![iface(0xFF, 0, 0)], ..Default::default() });
        assert_eq!(c.kind, "nfc_reader");
        assert_eq!(c.name, "ACS ACR122U");
        let c = classify(&Raw { vid: 0x0529, pid: 0x0600, interfaces: vec![iface(0x0B, 0, 0)], ..Default::default() });
        assert_eq!(c.kind, "security_token");
        let c = classify(&Raw { vid: 0x9999, pid: 1, hid: vec![hid(0xF1D0, 1)], ..Default::default() });
        assert_eq!(c.kind, "fido_key");
    }

    #[test]
    fn other_kinds() {
        assert_eq!(classify(&Raw { interfaces: vec![iface(8, 6, 0x50)], ..Default::default() }).kind, "storage");
        assert_eq!(classify(&Raw { interfaces: vec![iface(0x0E, 1, 0), iface(1, 1, 0)], ..Default::default() }).kind, "camera");
        assert_eq!(classify(&Raw { interfaces: vec![iface(7, 1, 2)], ..Default::default() }).kind, "printer");
        assert_eq!(classify(&Raw { class: 9, ..Default::default() }).kind, "hub");
        assert_eq!(classify(&Raw { vid: 0x05BA, pid: 0x000A, interfaces: vec![iface(0xFF, 0, 0)], ..Default::default() }).kind, "fingerprint_reader");
        assert_eq!(classify(&Raw { vid: 0x0403, pid: 0x6001, ports: vec!["COM3".into()], ..Default::default() }).kind, "serial");
        let c = classify(&Raw { vid: 0x0C27, pid: 0x3BFA, hid: vec![hid(1, 6)], ..Default::default() });
        assert_eq!(c.kind, "rfid_reader");
        assert_eq!(c.modes, vec!["keyboard"]);
        let c = classify(&Raw { vid: 0x0801, pid: 2, hid: vec![hid(0xFF00, 1)], ..Default::default() });
        assert_eq!(c.kind, "msr");
        assert_eq!(c.modes, vec!["hid"]);
    }
}
