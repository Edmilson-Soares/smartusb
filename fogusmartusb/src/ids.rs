//! Base de identificação por VID/PID: tabela embutida (fabricantes e modelos
//! conhecidos), base do usuário em JSON e o `usb.ids` do linux-usb.org.
//!
//! Precedência: usuário > embutida > usb.ids. A base do usuário e a embutida
//! podem dar o tipo (`kind`) do aparelho; o usb.ids só dá nomes.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use serde_json::Value;

/// Uma entrada: `pid = None` vale para todos os produtos do fabricante.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub vid: u16,
    pub pid: Option<u16>,
    pub vendor: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
}

#[derive(Default)]
struct Db {
    user: Vec<Entry>,
    vendors: HashMap<u16, String>,
    products: HashMap<(u16, u16), String>,
}

static DB: LazyLock<RwLock<Db>> = LazyLock::new(Default::default);

/// Tipo sugerido pelo fabricante: só vale quando a interface combina (um
/// fabricante de scanners que aparece como teclado é um scanner; um
/// fabricante de leitores de cartão que aparece como CCID é leitor).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hint {
    None,
    Scanner,
    SmartCard,
    Token,
    Fingerprint,
    Rfid,
    Msr,
    Printer,
    SerialBridge,
    Network,
}

/// (VID, fabricante, dica). Só fabricantes com VID conferido.
const VENDORS: &[(u16, &str, Hint)] = &[
    // leitores de código de barras / QR
    (0x05E0, "Zebra (Symbol)", Hint::Scanner),
    (0x0C2E, "Honeywell (Metrologic)", Hint::Scanner),
    (0x0536, "Honeywell (Hand Held Products)", Hint::Scanner),
    (0x05F9, "Datalogic", Hint::Scanner),
    (0x1EAB, "Newland", Hint::Scanner),
    (0x065A, "Opticon", Hint::Scanner),
    // smart card e tokens
    (0x072F, "ACS (Advanced Card Systems)", Hint::SmartCard),
    (0x076B, "HID Global (OMNIKEY)", Hint::SmartCard),
    (0x04E6, "Identiv (SCM Microsystems)", Hint::SmartCard),
    (0x08E6, "Gemalto (Gemplus)", Hint::SmartCard),
    (0x096E, "Feitian", Hint::SmartCard),
    (0x0C4B, "REINER SCT", Hint::SmartCard),
    (0x0DC3, "Athena Smartcard", Hint::SmartCard),
    (0x0B97, "O2 Micro", Hint::SmartCard),
    (0x0416, "Winbond / Nuvoton", Hint::None),
    (0x0529, "SafeNet (Aladdin)", Hint::Token),
    (0x24DC, "Aladdin R.D. (JaCarta)", Hint::Token),
    (0x0A89, "Aktiv (Rutoken)", Hint::Token),
    (0x1050, "Yubico", Hint::Token),
    (0x163C, "Watchdata", Hint::Token),
    (0x20A0, "Nitrokey (Clay Logic)", Hint::Token),
    // biometria
    (0x05BA, "DigitalPersona (HID)", Hint::Fingerprint),
    (0x1162, "SecuGen", Hint::Fingerprint),
    (0x1491, "Futronic", Hint::Fingerprint),
    (0x27C6, "Goodix", Hint::Fingerprint),
    (0x138A, "Validity Sensors", Hint::Fingerprint),
    (0x147E, "UPEK", Hint::Fingerprint),
    (0x08FF, "AuthenTec", Hint::Fingerprint),
    (0x1C7A, "EgisTec", Hint::Fingerprint),
    // RFID por teclado, tarja magnética
    (0x0C27, "rf IDEAS", Hint::Rfid),
    (0x0801, "MagTek", Hint::Msr),
    // impressoras de cupom
    (0x04B8, "Seiko Epson", Hint::Printer),
    (0x0519, "Star Micronics", Hint::Printer),
    (0x0DD4, "Custom Engineering", Hint::Printer),
    (0x1504, "Bixolon", Hint::Printer),
    // conversores USB-serial (balanças, scanners em modo COM, CLPs...)
    (0x0403, "FTDI", Hint::SerialBridge),
    (0x10C4, "Silicon Labs", Hint::SerialBridge),
    (0x1A86, "QinHeng (WCH)", Hint::SerialBridge),
    (0x067B, "Prolific", Hint::SerialBridge),
    // fabricantes gerais (só nome)
    (0x03F0, "HP", Hint::None),
    (0x0424, "Microchip (SMSC)", Hint::None),
    (0x045E, "Microsoft", Hint::None),
    (0x046A, "Cherry", Hint::None),
    (0x046D, "Logitech", Hint::None),
    (0x0483, "STMicroelectronics", Hint::None),
    (0x04CA, "Lite-On", Hint::None),
    (0x04D8, "Microchip", Hint::None),
    (0x04D9, "Holtek", Hint::None),
    (0x04E8, "Samsung", Hint::None),
    (0x04F2, "Chicony", Hint::None),
    (0x04F3, "Elan", Hint::None),
    (0x054C, "Sony", Hint::None),
    (0x058F, "Alcor Micro", Hint::None),
    (0x05AC, "Apple", Hint::None),
    (0x05E3, "Genesys Logic", Hint::None),
    (0x06CB, "Synaptics", Hint::None),
    (0x0781, "SanDisk", Hint::None),
    (0x0951, "Kingston", Hint::None),
    (0x0A5C, "Broadcom", Hint::None),
    (0x0B95, "ASIX", Hint::Network),
    (0x0BDA, "Realtek", Hint::None),
    (0x0C45, "Microdia", Hint::None),
    (0x0CF3, "Qualcomm Atheros", Hint::None),
    (0x12D1, "Huawei", Hint::None),
    (0x13D3, "IMC Networks", Hint::None),
    (0x1532, "Razer", Hint::None),
    (0x17EF, "Lenovo", Hint::None),
    (0x18D1, "Google", Hint::None),
    (0x1A2C, "China Resource Semico", Hint::None),
    (0x1BCF, "Sunplus", Hint::None),
    (0x1D6B, "Linux Foundation", Hint::None),
    (0x1FC9, "NXP", Hint::None),
    (0x2109, "VIA Labs", Hint::None),
    (0x22B8, "Motorola", Hint::None),
    (0x2341, "Arduino", Hint::None),
    (0x2717, "Xiaomi", Hint::None),
    (0x2E8A, "Raspberry Pi", Hint::None),
    (0x303A, "Espressif", Hint::None),
    (0x413C, "Dell", Hint::None),
    (0x5986, "Bison (Acer)", Hint::None),
    (0x8086, "Intel", Hint::None),
    (0x8087, "Intel", Hint::None),
];

/// (VID, PID, modelo, tipo).
const PRODUCTS: &[(u16, u16, &str, &str)] = &[
    (0x072F, 0x2200, "ACS ACR122U", "nfc_reader"),
    // leitor NFC barato (NSCCN/wCopy): HID próprio sobre PN532, sem PC/SC; veja wcopy.fg
    (0x0416, 0xB030, "wCopy Smart Reader (NSCCN)", "nfc_reader"),
    (0x072F, 0x223B, "ACS ACR1252U", "nfc_reader"),
    (0x072F, 0x90CC, "ACS ACR38U", "smartcard_reader"),
    (0x076B, 0x3021, "HID OMNIKEY 3121", "smartcard_reader"),
    (0x076B, 0x5422, "HID OMNIKEY 5422", "nfc_reader"),
    (0x076B, 0x5427, "HID OMNIKEY 5427 CK", "nfc_reader"),
    (0x04E6, 0x5116, "Identiv SCR3310 v2", "smartcard_reader"),
    (0x04E6, 0xE001, "SCM SCR331", "smartcard_reader"),
    (0x08E6, 0x3437, "Gemalto GemPC Twin", "smartcard_reader"),
    (0x08E6, 0x3438, "Gemalto GemPC Key", "smartcard_reader"),
    (0x0529, 0x0620, "SafeNet eToken 5110", "security_token"),
    (0x0A89, 0x0030, "Rutoken ECP", "security_token"),
    (0x1050, 0x0407, "YubiKey (OTP+FIDO+CCID)", "security_token"),
    (0x05BA, 0x000A, "DigitalPersona U.are.U 4500", "fingerprint_reader"),
];

pub fn hint(vid: u16) -> Hint {
    VENDORS.iter().find(|v| v.0 == vid).map(|v| v.2).unwrap_or(Hint::None)
}

/// Resultado da busca: nomes e tipo, com a fonte de cada um.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lookup {
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub kind: Option<String>,
    pub kind_source: Option<&'static str>,
    pub name_source: Option<&'static str>,
}

pub fn lookup(vid: u16, pid: u16) -> Lookup {
    let db = DB.read().unwrap();
    let mut out = Lookup::default();

    // Base do usuário: produto exato antes de fabricante inteiro.
    let user = db
        .user
        .iter()
        .find(|e| e.vid == vid && e.pid == Some(pid))
        .or_else(|| db.user.iter().find(|e| e.vid == vid && e.pid.is_none()));
    if let Some(e) = user {
        out.vendor = e.vendor.clone();
        out.product = e.name.clone();
        if out.product.is_some() {
            out.name_source = Some("user");
        }
        if let Some(k) = &e.kind {
            out.kind = Some(k.clone());
            out.kind_source = Some("user");
        }
    }

    if let Some(p) = PRODUCTS.iter().find(|p| p.0 == vid && p.1 == pid) {
        if out.product.is_none() {
            out.product = Some(p.2.to_string());
            out.name_source = Some("builtin");
        }
        if out.kind.is_none() {
            out.kind = Some(p.3.to_string());
            out.kind_source = Some("builtin");
        }
    }
    if out.vendor.is_none() {
        out.vendor = VENDORS.iter().find(|v| v.0 == vid).map(|v| v.1.to_string());
    }

    if out.vendor.is_none() {
        out.vendor = db.vendors.get(&vid).cloned();
    }
    if out.product.is_none()
        && let Some(p) = db.products.get(&(vid, pid))
    {
        out.product = Some(p.clone());
        out.name_source = Some("usbids");
    }
    out
}

/// Número em hex (`"0c2e"`, `"0x0C2E"`) ou inteiro.
pub fn parse_id(v: &Value) -> Option<u16> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
        Value::String(s) => {
            let s = s.trim();
            let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
            u16::from_str_radix(s, 16).ok()
        }
        _ => None,
    }
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Base do usuário: `[{ "vid", "pid"?, "name"?, "vendor"?, "kind"? }]`.
pub fn parse_user_json(text: &str) -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let list = v.as_array().ok_or("the device list must be a JSON list")?;
    list.iter()
        .enumerate()
        .map(|(i, e)| {
            let vid = e.get("vid").and_then(parse_id).ok_or_else(|| format!("entry {i}: missing or invalid \"vid\""))?;
            let pid = match e.get("pid") {
                None | Some(Value::Null) => None,
                Some(p) => Some(parse_id(p).ok_or_else(|| format!("entry {i}: invalid \"pid\""))?),
            };
            Ok(Entry { vid, pid, vendor: opt_str(e, "vendor"), name: opt_str(e, "name"), kind: opt_str(e, "kind") })
        })
        .collect()
}

type UsbIds = (HashMap<u16, String>, HashMap<(u16, u16), String>);

/// Formato `usb.ids`: fabricante em `"vvvv  nome"`, produto em `"\tpppp  nome"`.
/// A lista de classes (`C xx ...`) no fim do arquivo é ignorada.
pub fn parse_usb_ids(text: &str) -> UsbIds {
    let mut vendors = HashMap::new();
    let mut products = HashMap::new();
    let mut current: Option<u16> = None;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('\t') {
            if rest.starts_with('\t') {
                continue; // interfaces
            }
            if let (Some(vid), Some((id, name))) = (current, split_id(rest)) {
                products.insert((vid, id), name.to_string());
            }
            continue;
        }
        match split_id(line) {
            Some((id, name)) => {
                vendors.insert(id, name.to_string());
                current = Some(id);
            }
            // "C 00  ...", "AT ...", "HID ..." etc.: acabaram os fabricantes.
            None => break,
        }
    }
    (vendors, products)
}

fn split_id(s: &str) -> Option<(u16, &str)> {
    let (id, name) = s.split_once(char::is_whitespace)?;
    if id.len() != 4 {
        return None;
    }
    Some((u16::from_str_radix(id, 16).ok()?, name.trim()))
}

/// Carrega `.json` (base do usuário) ou um `usb.ids`. Retorna quantas
/// entradas entraram.
pub fn load_file(path: &str) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    // O usb.ids é UTF-8, mas versões antigas tinham Latin-1.
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };
    let mut db = DB.write().unwrap();
    if path.to_ascii_lowercase().ends_with(".json") {
        let entries = parse_user_json(&text)?;
        let n = entries.len();
        // Entradas novas na frente: a última base carregada ganha.
        db.user.splice(0..0, entries);
        Ok(n)
    } else {
        let (v, p) = parse_usb_ids(&text);
        if v.is_empty() {
            return Err(format!("{path}: no vendors found (expected usb.ids format or .json)"));
        }
        let n = v.len() + p.len();
        db.vendors.extend(v);
        db.products.extend(p);
        Ok(n)
    }
}

pub fn clear() {
    *DB.write().unwrap() = Db::default();
}

pub fn count() -> usize {
    let db = DB.read().unwrap();
    db.user.len() + db.vendors.len() + db.products.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_ids_format() {
        let text = "# comment\n0403  Future Technology Devices International, Ltd\n\t6001  FT232 Serial (UART) IC\n\t\t00  iface\n0c2e  Metrologic Instruments\n\t0b61  Vuquest 3310g\nC 00  (Defined at Interface level)\n\t01  Audio\n";
        let (v, p) = parse_usb_ids(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[&0x0403], "Future Technology Devices International, Ltd");
        assert_eq!(p[&(0x0C2E, 0x0B61)], "Vuquest 3310g");
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn user_json() {
        let e = parse_user_json(r#"[{"vid":"0x0c2e","pid":"0b61","name":"Caixa 1","kind":"barcode_scanner"},{"vid":1234}]"#).unwrap();
        assert_eq!(e[0].vid, 0x0C2E);
        assert_eq!(e[0].pid, Some(0x0B61));
        assert_eq!(e[1].vid, 1234);
        assert_eq!(e[1].pid, None);
        assert!(parse_user_json(r#"[{"pid":1}]"#).is_err());
    }

    #[test]
    fn builtin() {
        let l = lookup(0x072F, 0x2200);
        assert_eq!(l.product.as_deref(), Some("ACS ACR122U"));
        assert_eq!(l.kind.as_deref(), Some("nfc_reader"));
        assert_eq!(hint(0x0C2E), Hint::Scanner);
        assert_eq!(lookup(0x0C2E, 1).vendor.as_deref(), Some("Honeywell (Metrologic)"));
    }
}
