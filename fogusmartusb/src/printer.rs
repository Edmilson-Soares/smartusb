//! Impressoras (cupom/térmicas ESC/POS e qualquer impressora que aceite
//! dados brutos): envio de bytes e leitura de status, por vários caminhos:
//!
//! - `spooler` (Windows): impressora instalada, pelo nome, em trabalho RAW.
//! - `usbprint` (Windows): a interface do driver de impressora USB do
//!   Windows (`\\?\usb#vid_...#{28d78fad-...}`), sem precisar instalar a
//!   impressora.
//! - `usb`: transferência bulk direta (nusb). Linux/macOS, e Windows quando
//!   o driver do aparelho é WinUSB (comum em impressoras de cupom baratas).
//! - `serial`: impressora em porta COM.
//! - `file`: `/dev/usb/lp0` (Linux).
//!
//! E um gerador de comandos ESC/POS (`escpos`) e a conversão de texto para
//! as páginas de código das impressoras (`encode`).

use std::io::{Read, Write};
use std::time::Duration;

use nusb::MaybeFuture;
use serde_json::{Value, json};

use crate::enumerate::{self, Device};

enum Backend {
    #[cfg(windows)]
    Spooler(String),
    File(std::fs::File),
    Serial(Box<dyn serialport::SerialPort>),
    Usb {
        _iface: nusb::Interface,
        writer: nusb::io::EndpointWrite<nusb::transfer::Bulk>,
        reader: Option<nusb::io::EndpointRead<nusb::transfer::Bulk>>,
    },
}

pub struct Printer {
    backend: Backend,
    pub kind: &'static str,
    pub target: String,
    pub device: Option<Device>,
}

fn is_port(sel: &str) -> bool {
    let up = sel.to_ascii_uppercase();
    (up.starts_with("COM") && up.len() > 3 && up[3..].chars().all(|c| c.is_ascii_digit())) || sel.starts_with("/dev/tty") || sel.starts_with("/dev/cu.")
}

impl Printer {
    /// `selector`: `""` (primeira impressora USB), id, `vvvv:pppp`, nome de
    /// impressora instalada (`"printer:EPSON TM-T20"` ou só o nome, no
    /// Windows), porta (`COM3`), `/dev/usb/lp0` ou caminho `\\?\usb#...`.
    /// `opts`: `{ backend, baud, timeout_ms }`.
    pub fn open(selector: &str, opts: &Value) -> Result<Printer, String> {
        let sel = selector.trim();
        let backend = opts.get("backend").and_then(Value::as_str).unwrap_or("auto");
        let baud = opts.get("baud").and_then(Value::as_u64).unwrap_or(9600) as u32;
        let timeout = Duration::from_millis(opts.get("timeout_ms").and_then(Value::as_u64).unwrap_or(5000));

        if let Some(name) = sel.strip_prefix("printer:").or(if backend == "spooler" { Some(sel) } else { None }) {
            return Self::spooler(name);
        }
        if is_port(sel) || backend == "serial" {
            let port = serialport::new(sel, baud).timeout(timeout).open().map_err(|e| format!("cannot open serial port {sel}: {e}"))?;
            return Ok(Printer { backend: Backend::Serial(port), kind: "serial", target: sel.into(), device: None });
        }
        if sel.starts_with("/dev/") || sel.starts_with(r"\\?\") || sel.starts_with(r"\\.\") || backend == "file" {
            let f = std::fs::OpenOptions::new().read(true).write(true).open(sel).map_err(|e| format!("cannot open {sel}: {e}"))?;
            return Ok(Printer { backend: Backend::File(f), kind: "file", target: sel.into(), device: None });
        }

        let dev = if sel.is_empty() {
            enumerate::list(false)?.into_iter().find(|d| d.class.kind == "printer").ok_or("no USB printer found")?
        } else {
            match enumerate::find(sel) {
                Ok(d) => d,
                // Pode ser o nome de uma impressora instalada.
                Err(e) => {
                    #[cfg(windows)]
                    if system_printers().iter().any(|p| p["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(sel))) {
                        return Self::spooler(sel);
                    }
                    return Err(e);
                }
            }
        };

        let mut errors = Vec::new();
        #[cfg(windows)]
        if matches!(backend, "auto" | "usbprint") {
            match usbprint_paths().into_iter().find(|p| path_matches(p, &dev)) {
                Some(path) => match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                    Ok(f) => return Ok(Printer { backend: Backend::File(f), kind: "usbprint", target: path, device: Some(dev) }),
                    Err(e) => errors.push(format!("usbprint: {e}")),
                },
                None => errors.push("usbprint: the device does not use the Windows USB printer driver".into()),
            }
        }
        if matches!(backend, "auto" | "usb") {
            match Self::usb(&dev, timeout) {
                Ok(b) => return Ok(Printer { backend: b, kind: "usb", target: dev.vid_pid(), device: Some(dev) }),
                Err(e) => errors.push(format!("usb: {e}")),
            }
        }
        if matches!(backend, "auto") && !dev.raw.ports.is_empty() {
            let p = &dev.raw.ports[0];
            if let Ok(port) = serialport::new(p, baud).timeout(timeout).open() {
                return Ok(Printer { backend: Backend::Serial(port), kind: "serial", target: p.clone(), device: Some(dev) });
            }
        }
        Err(format!(
            "cannot open printer {} ({}): {}{}",
            dev.class.name,
            dev.vid_pid(),
            errors.join("; "),
            if cfg!(windows) { "; install it in Windows and use \"printer:<name>\" (see printers())" } else { "" }
        ))
    }

    #[cfg(windows)]
    fn spooler(name: &str) -> Result<Printer, String> {
        if !system_printers().iter().any(|p| p["name"].as_str() == Some(name)) {
            return Err(format!("no installed printer named \"{name}\""));
        }
        Ok(Printer { backend: Backend::Spooler(name.to_string()), kind: "spooler", target: name.to_string(), device: None })
    }

    #[cfg(not(windows))]
    fn spooler(_name: &str) -> Result<Printer, String> {
        Err("printing through the system spooler is only available on Windows (on Linux use the device: id, vid:pid or /dev/usb/lp0)".into())
    }

    fn usb(dev: &Device, timeout: Duration) -> Result<Backend, String> {
        let info = nusb::list_devices()
            .wait()
            .map_err(|e| e.to_string())?
            .find(|d| d.vendor_id() == dev.raw.vid && d.product_id() == dev.raw.pid && (dev.serial.is_none() || d.serial_number() == dev.serial.as_deref()))
            .ok_or("device disappeared")?;
        let handle = info.open().wait().map_err(|e| format!("cannot open USB device: {e}"))?;
        let config = handle.active_configuration().map_err(|e| format!("cannot read configuration: {e}"))?;
        // Interface de impressora (classe 7) ou, nas de fabricante, a primeira com bulk OUT.
        let mut best: Option<(u8, u8, Option<u8>, bool)> = None;
        for alt in config.interface_alt_settings() {
            let out = alt.endpoints().find(|e| e.transfer_type() == nusb::descriptors::TransferType::Bulk && e.address() & 0x80 == 0);
            let inp = alt.endpoints().find(|e| e.transfer_type() == nusb::descriptors::TransferType::Bulk && e.address() & 0x80 != 0);
            if let Some(o) = out {
                let printer_class = alt.class() == 7;
                if best.is_none_or(|b| printer_class && !b.3) {
                    best = Some((alt.interface_number(), o.address(), inp.map(|i| i.address()), printer_class));
                }
            }
        }
        let (n, out, inp, _) = best.ok_or("no bulk OUT endpoint")?;
        #[cfg(target_os = "linux")]
        let iface = handle.detach_and_claim_interface(n).wait();
        #[cfg(not(target_os = "linux"))]
        let iface = handle.claim_interface(n).wait();
        let iface = iface.map_err(|e| format!("cannot claim interface {n}: {e}"))?;
        let mut writer = iface.endpoint::<nusb::transfer::Bulk, nusb::transfer::Out>(out).map_err(|e| e.to_string())?.writer(4096);
        writer.set_write_timeout(timeout);
        let reader = match inp {
            Some(a) => iface.endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(a).ok().map(|ep| ep.reader(64)),
            None => None,
        };
        Ok(Backend::Usb { _iface: iface, writer, reader })
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        match &mut self.backend {
            #[cfg(windows)]
            Backend::Spooler(name) => spool(name, data),
            Backend::File(f) => {
                f.write_all(data).map_err(|e| format!("printer write failed: {e}"))?;
                f.flush().map_err(|e| format!("printer write failed: {e}"))?;
                Ok(data.len())
            }
            Backend::Serial(p) => {
                p.write_all(data).map_err(|e| format!("printer write failed: {e}"))?;
                p.flush().map_err(|e| format!("printer write failed: {e}"))?;
                Ok(data.len())
            }
            Backend::Usb { writer, .. } => {
                writer.write_all(data).map_err(|e| format!("printer write failed: {e}"))?;
                writer.flush().map_err(|e| format!("printer write failed: {e}"))?;
                Ok(data.len())
            }
        }
    }

    /// Lê até `max` bytes (respostas de status). Vazio se o tempo acabar.
    pub fn read(&mut self, timeout: Duration, max: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; max.clamp(1, 4096)];
        let r = match &mut self.backend {
            Backend::Serial(p) => {
                p.set_timeout(timeout).map_err(|e| e.to_string())?;
                p.read(&mut buf)
            }
            Backend::Usb { reader: Some(r), .. } => {
                r.set_read_timeout(timeout);
                r.read(&mut buf)
            }
            Backend::Usb { reader: None, .. } => return Err("this printer has no IN endpoint (no status)".into()),
            _ => return Err(format!("reading status is not supported with the {} backend (use usb or serial)", self.kind)),
        };
        match r {
            Ok(n) => {
                buf.truncate(n);
                Ok(buf)
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Vec::new()),
            Err(e) => Err(format!("printer read failed: {e}")),
        }
    }

    pub fn info(&self) -> Value {
        json!({ "backend": self.kind, "target": self.target, "device": self.device.as_ref().map(|d| d.to_json()) })
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
unsafe fn pwstr(p: *const u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) }))
}

/// Um trabalho RAW no spooler do Windows.
#[cfg(windows)]
fn spool(name: &str, data: &[u8]) -> Result<usize, String> {
    use windows_sys::Win32::Graphics::Printing::*;
    let err = |what: &str| format!("{what} failed: {}", std::io::Error::last_os_error());
    unsafe {
        let mut h: PRINTER_HANDLE = std::mem::zeroed();
        let wname = wide(name);
        if OpenPrinterW(wname.as_ptr(), &mut h, std::ptr::null()) == 0 {
            return Err(err("OpenPrinter"));
        }
        let mut doc_name = wide("fogusmartusb");
        let mut datatype = wide("RAW");
        let doc = DOC_INFO_1W { pDocName: doc_name.as_mut_ptr(), pOutputFile: std::ptr::null_mut(), pDatatype: datatype.as_mut_ptr() };
        let result = (|| {
            if StartDocPrinterW(h, 1, &doc) == 0 {
                return Err(err("StartDocPrinter"));
            }
            StartPagePrinter(h);
            let mut written = 0u32;
            let ok = WritePrinter(h, data.as_ptr().cast(), data.len() as u32, &mut written);
            EndPagePrinter(h);
            EndDocPrinter(h);
            if ok == 0 { Err(err("WritePrinter")) } else { Ok(written as usize) }
        })();
        ClosePrinter(h);
        result
    }
}

/// Impressoras instaladas no Windows: `[{ name, port, driver, default }]`.
#[cfg(windows)]
pub fn system_printers() -> Vec<Value> {
    use windows_sys::Win32::Graphics::Printing::*;
    unsafe {
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let (mut needed, mut count) = (0u32, 0u32);
        EnumPrintersW(flags, std::ptr::null(), 2, std::ptr::null_mut(), 0, &mut needed, &mut count);
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        if EnumPrintersW(flags, std::ptr::null(), 2, buf.as_mut_ptr().cast(), needed, &mut needed, &mut count) == 0 {
            return Vec::new();
        }
        let list = std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_2W, count as usize);
        list.iter()
            .map(|p| {
                json!({
                    "name": pwstr(p.pPrinterName),
                    "port": pwstr(p.pPortName),
                    "driver": pwstr(p.pDriverName),
                    "default": p.Attributes & PRINTER_ATTRIBUTE_DEFAULT != 0,
                })
            })
            .collect()
    }
}

#[cfg(not(windows))]
pub fn system_printers() -> Vec<Value> {
    Vec::new()
}

/// Interfaces do driver de impressora USB do Windows (usbprint.sys).
#[cfg(windows)]
pub fn usbprint_paths() -> Vec<String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
    let guid = windows_sys::core::GUID::from_u128(0x28d78fad_5a12_11d1_ae5b_0000f803a8c2);
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(&mut len, &guid, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(&guid, std::ptr::null(), buf.as_mut_ptr(), len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS {
            return Vec::new();
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

#[cfg(not(windows))]
pub fn usbprint_paths() -> Vec<String> {
    Vec::new()
}

#[cfg_attr(not(windows), allow(dead_code))]
fn path_matches(path: &str, dev: &Device) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains(&format!("vid_{:04x}&pid_{:04x}", dev.raw.vid, dev.raw.pid)) && dev.serial.as_ref().is_none_or(|s| p.contains(&s.to_ascii_lowercase()) || !p.contains('#'))
}

// ---- páginas de código -----------------------------------------------------

const CP437: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
const CP850: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}";
const CP860: &str = "ÇüéâãàÁçêÊèÍÔìÃÂÉÀÈôõòÚùÌÕÜ¢£Ù₧ÓáíóúñÑªº¿Ò¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
const CP1252_80: &str = "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ\u{8d}Ž\u{8f}\u{90}‘’“”•–—˜™š›œ\u{9d}žŸ";

/// Número da tabela no comando ESC t (padrão Epson).
fn codepage_number(cp: &str) -> Option<u8> {
    Some(match cp {
        "cp437" => 0,
        "cp850" => 2,
        "cp860" => 3,
        "cp1252" | "latin1" => 16,
        _ => return None,
    })
}

fn normalize_cp(cp: &str) -> String {
    let c = cp.to_ascii_lowercase().replace(['-', '_', ' '], "");
    match c.as_str() {
        "" | "437" | "pc437" | "cp437" => "cp437".into(),
        "850" | "pc850" | "cp850" => "cp850".into(),
        "860" | "pc860" | "cp860" => "cp860".into(),
        "1252" | "wpc1252" | "cp1252" | "windows1252" => "cp1252".into(),
        "latin1" | "iso88591" => "latin1".into(),
        "utf8" => "utf8".into(),
        other => other.into(),
    }
}

/// Texto para bytes na página de código da impressora; o que não existe
/// nela vira `?` (acentos caem para a letra sem acento antes disso).
pub fn encode(text: &str, codepage: &str) -> Result<Vec<u8>, String> {
    let cp = normalize_cp(codepage);
    if cp == "utf8" {
        return Ok(text.as_bytes().to_vec());
    }
    let high: Option<&str> = match cp.as_str() {
        "cp437" => Some(CP437),
        "cp850" => Some(CP850),
        "cp860" => Some(CP860),
        "cp1252" | "latin1" => None,
        other => return Err(format!("unknown code page \"{other}\" (cp437, cp850, cp860, cp1252, latin1, utf8)")),
    };
    let one = |c: char| -> Option<u8> {
        if (c as u32) < 0x80 {
            return Some(c as u8);
        }
        match high {
            Some(t) => t.chars().position(|x| x == c).map(|i| 0x80 + i as u8),
            None if cp == "cp1252" => CP1252_80.chars().position(|x| x == c).map(|i| 0x80 + i as u8).or(((c as u32) >= 0xA0 && (c as u32) <= 0xFF).then_some(c as u8)),
            None => ((c as u32) <= 0xFF).then_some(c as u8),
        }
    };
    Ok(text
        .chars()
        .map(|c| one(c).or_else(|| one(strip_accent(c))).unwrap_or(b'?'))
        .collect())
}

fn strip_accent(c: char) -> char {
    const FROM: &str = "áàâãäåéèêëíìîïóòôõöúùûüçñýÿÁÀÂÃÄÅÉÈÊËÍÌÎÏÓÒÔÕÖÚÙÛÜÇÑÝ";
    const TO: &str = "aaaaaaeeeeiiiiooooouuuucnyyAAAAAAEEEEIIIIOOOOOUUUUCNY";
    FROM.chars().position(|x| x == c).and_then(|i| TO.chars().nth(i)).unwrap_or(c)
}

// ---- ESC/POS ----------------------------------------------------------------

/// Gera os bytes ESC/POS de uma lista de comandos, por exemplo:
/// `[{"init":true},{"codepage":"cp860"},{"align":"center"},{"bold":true},
///   {"text":"LOJA\n"},{"qr":"https://...","size":6},{"barcode":"789...","type":"ean13"},
///   {"feed":3},{"cut":true},{"drawer":true}]`
pub fn escpos(cmds: &Value) -> Result<Vec<u8>, String> {
    let list = cmds.as_array().ok_or("ESC/POS commands must be a JSON list")?;
    let mut out = Vec::new();
    let mut cp = "cp437".to_string();
    for (i, c) in list.iter().enumerate() {
        let err = |m: &str| format!("command {i}: {m}");
        let obj = c.as_object().ok_or_else(|| err("each command must be an object"))?;
        let num = |k: &str, d: u64| obj.get(k).and_then(Value::as_u64).unwrap_or(d);
        let on = |k: &str| obj.get(k).map(|v| v.as_bool().unwrap_or(true) as u8);
        let mut known = false;
        if obj.contains_key("init") {
            out.extend_from_slice(&[0x1B, 0x40]);
            known = true;
        }
        if let Some(v) = obj.get("codepage").and_then(Value::as_str) {
            cp = normalize_cp(v);
            if let Some(n) = codepage_number(&cp) {
                out.extend_from_slice(&[0x1B, 0x74, n]);
            }
            known = true;
        }
        if let Some(a) = obj.get("align").and_then(Value::as_str) {
            let n = match a {
                "left" => 0,
                "center" => 1,
                "right" => 2,
                _ => return Err(err("align must be left, center or right")),
            };
            out.extend_from_slice(&[0x1B, 0x61, n]);
            known = true;
        }
        if let Some(b) = on("bold") {
            out.extend_from_slice(&[0x1B, 0x45, b]);
            known = true;
        }
        if let Some(b) = on("underline") {
            out.extend_from_slice(&[0x1B, 0x2D, b]);
            known = true;
        }
        if let Some(b) = on("invert") {
            out.extend_from_slice(&[0x1D, 0x42, b]);
            known = true;
        }
        if let Some(s) = obj.get("size") {
            // [largura, altura] de 1 a 8, ou um número para os dois.
            let (w, h) = match s {
                Value::Array(a) if a.len() == 2 => (a[0].as_u64().unwrap_or(1), a[1].as_u64().unwrap_or(1)),
                Value::Number(n) => (n.as_u64().unwrap_or(1), n.as_u64().unwrap_or(1)),
                _ => return Err(err("size must be a number or [width, height]")),
            };
            if !(1..=8).contains(&w) || !(1..=8).contains(&h) {
                return Err(err("size must be 1..8"));
            }
            if !obj.contains_key("qr") {
                out.extend_from_slice(&[0x1D, 0x21, (((w - 1) << 4) | (h - 1)) as u8]);
                known = true;
            }
        }
        if let Some(t) = obj.get("text").and_then(Value::as_str) {
            out.extend(encode(t, &cp)?);
            known = true;
        }
        if let Some(t) = obj.get("line").and_then(Value::as_str) {
            out.extend(encode(t, &cp)?);
            out.push(b'\n');
            known = true;
        }
        if let Some(h) = obj.get("raw").and_then(Value::as_str) {
            out.extend(crate::parse_hex(h)?);
            known = true;
        }
        if let Some(q) = obj.get("qr").and_then(Value::as_str) {
            let size = num("size", 6).clamp(1, 16) as u8;
            let ec = match obj.get("ec").and_then(Value::as_str).unwrap_or("m") {
                "l" | "L" => 48,
                "m" | "M" => 49,
                "q" | "Q" => 50,
                "h" | "H" => 51,
                _ => return Err(err("ec must be l, m, q or h")),
            };
            let data = q.as_bytes();
            let n = data.len() + 3;
            if n > 7092 {
                return Err(err("QR data too long"));
            }
            out.extend_from_slice(&[0x1D, 0x28, 0x6B, 4, 0, 49, 65, 50, 0]); // modelo 2
            out.extend_from_slice(&[0x1D, 0x28, 0x6B, 3, 0, 49, 67, size]);
            out.extend_from_slice(&[0x1D, 0x28, 0x6B, 3, 0, 49, 69, ec]);
            out.extend_from_slice(&[0x1D, 0x28, 0x6B, (n % 256) as u8, (n / 256) as u8, 49, 80, 48]);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0x1D, 0x28, 0x6B, 3, 0, 49, 81, 48]);
            known = true;
        }
        if let Some(b) = obj.get("barcode").and_then(Value::as_str) {
            let t = obj.get("type").and_then(Value::as_str).unwrap_or("code128");
            let m: u8 = match t {
                "upca" => 65,
                "upce" => 66,
                "ean13" => 67,
                "ean8" => 68,
                "code39" => 69,
                "itf" => 70,
                "codabar" => 71,
                "code93" => 72,
                "code128" => 73,
                _ => return Err(err("barcode type must be upca, upce, ean13, ean8, code39, itf, codabar, code93 or code128")),
            };
            let mut data = b.as_bytes().to_vec();
            if m == 73 && !b.starts_with('{') {
                data.splice(0..0, *b"{B");
            }
            if data.len() > 255 {
                return Err(err("barcode data too long"));
            }
            out.extend_from_slice(&[0x1D, 0x68, num("height", 80).clamp(1, 255) as u8]);
            out.extend_from_slice(&[0x1D, 0x77, num("width", 2).clamp(2, 6) as u8]);
            let hri = match obj.get("hri").and_then(Value::as_str).unwrap_or("below") {
                "none" => 0,
                "above" => 1,
                "both" => 3,
                _ => 2,
            };
            out.extend_from_slice(&[0x1D, 0x48, hri, 0x1D, 0x6B, m, data.len() as u8]);
            out.extend(data);
            known = true;
        }
        if obj.contains_key("feed") {
            out.extend_from_slice(&[0x1B, 0x64, num("feed", 1).min(255) as u8]);
            known = true;
        }
        if let Some(c) = obj.get("cut") {
            // GS V 66 n: avança até a guilhotina e corta (parcial com "partial").
            let partial = c.as_str() == Some("partial");
            out.extend_from_slice(&[0x1D, 0x56, if partial { 66 } else { 65 }, 3]);
            known = true;
        }
        if obj.contains_key("drawer") {
            let pin = if num("pin", 2) == 5 { 1 } else { 0 };
            out.extend_from_slice(&[0x1B, 0x70, pin, 25, 250]);
            known = true;
        }
        if obj.contains_key("beep") {
            out.extend_from_slice(&[0x1B, 0x42, num("beep", 1).clamp(1, 9) as u8, 2]);
            known = true;
        }
        if !known {
            return Err(err(&format!("unknown command {c}")));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_pages() {
        assert_eq!(CP437.chars().count(), 128);
        assert_eq!(CP850.chars().count(), 128);
        assert_eq!(CP860.chars().count(), 128);
        assert_eq!(encode("Ação", "cp860").unwrap(), vec![b'A', 0x87, 0x84, b'o']);
        assert_eq!(encode("Ação", "cp850").unwrap(), vec![b'A', 0x87, 0xC6, b'o']);
        // CP437 não tem ã: cai para "a".
        assert_eq!(encode("Ação", "cp437").unwrap(), vec![b'A', 0x87, b'a', b'o']);
        assert_eq!(encode("€", "cp1252").unwrap(), vec![0x80]);
        assert_eq!(encode("日", "latin1").unwrap(), vec![b'?']);
        assert!(encode("x", "ebcdic").is_err());
    }

    #[test]
    fn escpos_commands() {
        let v = serde_json::json!([
            {"init": true}, {"codepage": "860"}, {"align": "center"}, {"bold": true},
            {"line": "Olá"}, {"size": [2, 1]}, {"qr": "abc", "size": 4}, {"barcode": "123", "type": "code128"},
            {"feed": 2}, {"cut": true}, {"drawer": true}
        ]);
        let b = escpos(&v).unwrap();
        assert_eq!(&b[..2], &[0x1B, 0x40]);
        assert_eq!(&b[2..5], &[0x1B, 0x74, 3]);
        assert_eq!(&b[5..8], &[0x1B, 0x61, 1]);
        assert_eq!(&b[8..11], &[0x1B, 0x45, 1]);
        assert_eq!(&b[11..15], &[b'O', b'l', 0xA0, b'\n']);
        assert_eq!(&b[15..18], &[0x1D, 0x21, 0x10]);
        // QR: armazena 3 + 3 bytes
        let store = b.windows(8).position(|w| w == [0x1D, 0x28, 0x6B, 6, 0, 49, 80, 48]).unwrap();
        assert_eq!(&b[store + 8..store + 11], b"abc");
        let bar = b.windows(3).position(|w| w == [0x1D, 0x6B, 73]).unwrap();
        assert_eq!(b[bar + 3], 5);
        assert_eq!(&b[bar + 4..bar + 9], b"{B123");
        assert!(b.ends_with(&[0x1B, 0x64, 2, 0x1D, 0x56, 65, 3, 0x1B, 0x70, 0, 25, 250]));
        assert!(escpos(&serde_json::json!([{"nope": 1}])).is_err());
        assert!(escpos(&serde_json::json!([{"align": "up"}])).is_err());
    }

    #[test]
    fn usbprint_path_matching() {
        let raw = crate::classify::Raw { vid: 0x04B8, pid: 0x0202, ..Default::default() };
        let dev = Device {
            id: "x".into(),
            class: crate::classify::classify(&raw),
            raw,
            serial: None,
            bus: String::new(),
            port_chain: String::new(),
            speed: None,
            usb_version: 0,
            device_version: 0,
            driver: None,
        };
        assert!(path_matches(r"\\?\usb#vid_04b8&pid_0202#5&abc#{28d78fad-5a12-11d1-ae5b-0000f803a8c2}", &dev));
        assert!(!path_matches(r"\\?\usb#vid_04b8&pid_0e15#5&abc#{28d78fad}", &dev));
    }
}
