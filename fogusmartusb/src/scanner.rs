//! Leitura de scanners (código de barras/QR, RFID por teclado, tarja
//! magnética...) com uma API só, qualquer que seja o modo do aparelho:
//!
//! - `hidpos`: HID POS (usage page 0x8C). Leitura exclusiva, sem digitar
//!   nada na janela em foco. Recomendado.
//! - `serial`: USB COM / CDC-ACM (porta COM, /dev/ttyACM*).
//! - `keyboard`: emulação de teclado. No Windows por Raw Input (separa o
//!   scanner do teclado de verdade); no Linux/macOS pelo hidraw/IOHID.
//! - `hid`: relatórios HID brutos de outros aparelhos (tarja, balança...).

use std::collections::HashMap;
use std::ffi::CString;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::content;
use crate::enumerate::{self, Device};
use crate::keymap::{Burst, Decoder, Layout};

#[derive(Clone, Debug)]
pub struct Opts {
    pub mode: String,
    pub layout: Layout,
    pub idle_ms: Option<u64>,
    pub tab_ends: bool,
    pub min_length: usize,
    /// Filtro de rajada (modo teclado): descarta leituras cujo intervalo
    /// médio entre teclas passe disso (digitação humana).
    pub max_gap_ms: Option<f64>,
    pub baud: u32,
    pub port: Option<String>,
    pub usage_page: Option<u16>,
    pub aim: bool,
    pub synthetic: bool,
    /// Modo teclado: as teclas do scanner não chegam aos programas (Windows).
    pub exclusive: bool,
}

impl Opts {
    pub fn from_json(v: &Value) -> Result<Opts, String> {
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        let n = |k: &str| v.get(k).and_then(Value::as_f64);
        let b = |k: &str, d: bool| v.get(k).and_then(Value::as_bool).unwrap_or(d);
        let mode = s("mode").unwrap_or("auto").to_ascii_lowercase();
        if !["auto", "hidpos", "serial", "keyboard", "hid"].contains(&mode.as_str()) {
            return Err(format!("unknown mode \"{mode}\" (use auto, hidpos, serial, keyboard or hid)"));
        }
        let max_gap_ms = match v.get("max_gap_ms") {
            Some(Value::Bool(false)) => Some(-1.0),
            _ => n("max_gap_ms"),
        };
        Ok(Opts {
            mode,
            layout: Layout::parse(s("layout").unwrap_or("us"))?,
            idle_ms: n("idle_ms").map(|x| x.max(1.0) as u64),
            tab_ends: s("terminator").is_some_and(|t| t.eq_ignore_ascii_case("tab")) || b("tab_ends", false),
            min_length: n("min_length").map(|x| x.max(0.0) as usize).unwrap_or(0),
            max_gap_ms,
            baud: n("baud").map(|x| x as u32).unwrap_or(9600),
            port: s("port").map(str::to_string),
            usage_page: n("usage_page").map(|x| x as u16),
            aim: b("aim", true),
            synthetic: b("synthetic", false),
            exclusive: b("exclusive", false),
        })
    }
}

/// Uma leitura, antes da análise de conteúdo.
pub struct Scan {
    pub text: String,
    pub raw: Vec<u8>,
    pub source: Option<String>,
    pub burst: Option<Burst>,
    /// HID POS: `"descriptor"` (layout declarado pelo aparelho) ou `"heuristic"`.
    pub decoder: Option<&'static str>,
}

enum Backend {
    Serial {
        port: Box<dyn serialport::SerialPort>,
        buf: Vec<u8>,
        last: Option<Instant>,
    },
    HidPos {
        dev: hidapi::HidDevice,
        layout: Option<crate::hiddesc::BarcodeLayout>,
    },
    Hid {
        dev: hidapi::HidDevice,
    },
    /// Teclados pelo HID (Linux/macOS): um ou vários.
    #[cfg_attr(windows, allow(dead_code))]
    HidKeyboard {
        devs: Vec<(hidapi::HidDevice, String, crate::keymap::BootReport, Decoder, Option<bool>)>,
    },
    #[cfg(windows)]
    RawInput {
        sub: crate::rawinput::Subscription,
        decoders: HashMap<String, Decoder>,
    },
}

pub struct Scanner {
    backend: Backend,
    pub mode: &'static str,
    pub device: Option<Device>,
    pub target: String,
    opts: Opts,
    idle_ms: u64,
    max_gap_ms: Option<f64>,
    min_length: usize,
}

fn is_port(sel: &str) -> bool {
    let up = sel.to_ascii_uppercase();
    (up.starts_with("COM") && up[3..].chars().all(|c| c.is_ascii_digit()) && up.len() > 3)
        || sel.starts_with("/dev/tty")
        || sel.starts_with("/dev/cu.")
}

fn is_hid_path(sel: &str) -> bool {
    sel.starts_with(r"\\?\HID") || sel.starts_with(r"\\?\hid") || sel.starts_with("/dev/hidraw") || sel.starts_with("IOService:") || sel.starts_with("DevSrvsID:")
}

fn open_hid_path(path: &str) -> Result<hidapi::HidDevice, String> {
    let api = hidapi::HidApi::new().map_err(|e| format!("hidapi: {e}"))?;
    let c = CString::new(path).map_err(|_| "invalid HID path".to_string())?;
    api.open_path(&c).map_err(|e| format!("cannot open HID device {path}: {e}"))
}

/// Layout HID POS pelo descritor de relatório do aparelho (None se o
/// descritor não estiver disponível ou não declarar "Decoded Data").
fn barcode_layout(dev: &hidapi::HidDevice) -> Option<crate::hiddesc::BarcodeLayout> {
    let mut buf = vec![0u8; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
    let n = dev.get_report_descriptor(&mut buf).ok()?;
    crate::hiddesc::barcode_layout(&crate::hiddesc::parse(&buf[..n]).ok()?)
}

fn hid_backend(dev: hidapi::HidDevice, mode: &str) -> Backend {
    if mode == "hid" {
        Backend::Hid { dev }
    } else {
        let layout = barcode_layout(&dev);
        Backend::HidPos { dev, layout }
    }
}

const SCANNER_KINDS: &[&str] = &["barcode_scanner", "rfid_reader", "msr", "scale"];

fn default_device() -> Result<Device, String> {
    let devs = enumerate::list(false)?;
    for kind in SCANNER_KINDS {
        if let Some(d) = devs.iter().find(|d| d.class.kind == *kind && !d.class.modes.is_empty()) {
            return Ok(d.clone());
        }
    }
    Err("no scanner found (use the selector \"keyboard\" to read scans from any keyboard)".into())
}

impl Scanner {
    pub fn open(selector: &str, opts: Opts) -> Result<Scanner, String> {
        let sel = selector.trim();
        let any_keyboard = matches!(sel.to_ascii_lowercase().as_str(), "keyboard" | "keyboards" | "*");

        if is_port(sel) || (opts.mode == "serial" && opts.port.is_some() && sel.is_empty()) {
            let port = if is_port(sel) { sel.to_string() } else { opts.port.clone().unwrap() };
            return Self::serial(&port, None, opts);
        }
        if is_hid_path(sel) {
            let dev = open_hid_path(sel)?;
            let mode = if opts.mode == "hid" { "hid" } else { "hidpos" };
            return Ok(Self::build(hid_backend(dev, mode), mode, None, sel.into(), opts));
        }
        if any_keyboard {
            return Self::keyboard(None, opts);
        }

        let dev = if sel.is_empty() { default_device()? } else { enumerate::find(sel)? };
        let mode: &str = if opts.mode == "auto" {
            dev.class.modes.first().copied().ok_or_else(|| format!("{} ({}) has no readable interface (HID POS, serial or keyboard)", dev.class.name, dev.vid_pid()))?
        } else {
            opts.mode.as_str()
        };
        match mode {
            "serial" => {
                let port = opts.port.clone().or_else(|| dev.raw.ports.first().cloned()).ok_or_else(|| format!("{} has no serial port", dev.class.name))?;
                Self::serial(&port, Some(dev), opts)
            }
            "hidpos" | "hid" => {
                let page = opts.usage_page;
                let col = dev
                    .raw
                    .hid
                    .iter()
                    .filter(|h| match (mode, page) {
                        (_, Some(p)) => h.usage_page == p,
                        ("hidpos", None) => h.usage_page == 0x8C,
                        _ => !(h.usage_page == 1 && matches!(h.usage, 2 | 6)),
                    })
                    .min_by_key(|h| (h.usage_page != 0x8C || h.usage != 0x02, h.interface))
                    .ok_or_else(|| format!("{} has no {} HID collection", dev.class.name, if mode == "hidpos" { "HID POS (0x8C)" } else { "usable" }))?;
                let hdev = open_hid_path(&col.path)?;
                let target = col.path.clone();
                let mode: &'static str = if mode == "hid" { "hid" } else { "hidpos" };
                let backend = hid_backend(hdev, mode);
                Ok(Self::build(backend, mode, Some(dev), target, opts))
            }
            "keyboard" => Self::keyboard(Some(dev), opts),
            other => Err(format!("unknown mode \"{other}\"")),
        }
    }

    fn build(backend: Backend, mode: &'static str, device: Option<Device>, target: String, opts: Opts) -> Scanner {
        let idle_ms = opts.idle_ms.unwrap_or(match mode {
            "serial" => 120,
            "keyboard" => 100,
            _ => 60,
        });
        // Teclado: com "qualquer teclado" o filtro de rajada vem ligado.
        let any = mode == "keyboard" && device.is_none();
        let max_gap_ms = match opts.max_gap_ms {
            Some(x) if x < 0.0 => None,
            Some(x) => Some(x),
            None if any => Some(35.0),
            None => None,
        };
        let min_length = if opts.min_length > 0 { opts.min_length } else if any { 4 } else { 1 };
        Scanner { backend, mode, device, target, opts, idle_ms, max_gap_ms, min_length }
    }

    fn serial(port: &str, device: Option<Device>, opts: Opts) -> Result<Scanner, String> {
        let p = serialport::new(port, opts.baud)
            .timeout(Duration::from_millis(20))
            .open()
            .map_err(|e| format!("cannot open serial port {port}: {e}"))?;
        // Alguns scanners CDC só mandam dados com DTR ligado.
        let mut p = p;
        let _ = p.write_data_terminal_ready(true);
        Ok(Self::build(Backend::Serial { port: p, buf: Vec::new(), last: None }, "serial", device, port.to_string(), opts))
    }

    #[cfg(windows)]
    fn keyboard(device: Option<Device>, opts: Opts) -> Result<Scanner, String> {
        let filter = device.as_ref().map(|d| (d.raw.vid, d.raw.pid));
        let sub = crate::rawinput::subscribe(filter, opts.synthetic, opts.exclusive)?;
        let target = device.as_ref().map(|d| d.vid_pid()).unwrap_or_else(|| "keyboard".into());
        Ok(Self::build(Backend::RawInput { sub, decoders: HashMap::new() }, "keyboard", device, target, opts))
    }

    #[cfg(not(windows))]
    fn keyboard(device: Option<Device>, opts: Opts) -> Result<Scanner, String> {
        if opts.exclusive {
            return Err("exclusive keyboard mode is only available on Windows; switch the scanner to HID POS or USB COM".into());
        }
        let api = hidapi::HidApi::new().map_err(|e| format!("hidapi: {e}"))?;
        let mut devs = Vec::new();
        let mut errors = Vec::new();
        for d in api.device_list() {
            if d.usage_page() != 1 || d.usage() != 6 {
                continue;
            }
            if let Some(dev) = &device
                && (d.vendor_id() != dev.raw.vid || d.product_id() != dev.raw.pid)
            {
                continue;
            }
            match d.open_device(&api) {
                Ok(h) => {
                    let id = format!("{:04x}:{:04x}", d.vendor_id(), d.product_id());
                    devs.push((h, id, crate::keymap::BootReport::default(), Decoder::new(opts.layout, opts.tab_ends), None));
                }
                Err(e) => errors.push(format!("{}: {e}", d.path().to_string_lossy())),
            }
        }
        if devs.is_empty() {
            return Err(format!(
                "cannot open any keyboard HID interface ({}); on Linux, give access to /dev/hidraw* (udev rule) or run as root; on macOS, allow Input Monitoring",
                if errors.is_empty() { "none found".to_string() } else { errors.join("; ") }
            ));
        }
        let target = device.as_ref().map(|d| d.vid_pid()).unwrap_or_else(|| "keyboard".into());
        Ok(Self::build(Backend::HidKeyboard { devs }, "keyboard", device, target, opts))
    }

    /// Aceita a leitura do teclado? (filtro de rajada e tamanho mínimo)
    fn accept(&self, b: &Burst) -> bool {
        if b.text.chars().count() < self.min_length {
            return false;
        }
        match self.max_gap_ms {
            Some(max) => b.keys > 1 && b.avg_gap_ms <= max,
            None => true,
        }
    }

    /// Espera uma leitura por até `timeout` (None = sem limite).
    pub fn read(&mut self, timeout: Option<Duration>) -> Result<Option<Scan>, String> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let left = |deadline: Option<Instant>| deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let idle = Duration::from_millis(self.idle_ms);
        loop {
            let slice = match left(deadline) {
                Some(d) if d.is_zero() => return Ok(None),
                Some(d) => d.min(Duration::from_millis(20)),
                None => Duration::from_millis(20),
            };
            match &mut self.backend {
                Backend::Serial { port, buf, last } => {
                    let mut chunk = [0u8; 1024];
                    match port.read(&mut chunk) {
                        Ok(0) => {}
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            *last = Some(Instant::now());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => return Err(format!("serial read failed: {e}")),
                    }
                    if let Some(i) = buf.iter().position(|&b| b == b'\r' || b == b'\n') {
                        let line: Vec<u8> = buf.drain(..i).collect();
                        while matches!(buf.first(), Some(b'\r' | b'\n')) {
                            buf.remove(0);
                        }
                        if !line.is_empty() {
                            return Ok(Some(Scan { text: bytes_text(&line), raw: line, source: None, burst: None, decoder: None }));
                        }
                    } else if !buf.is_empty() && last.is_some_and(|t| t.elapsed() >= idle) {
                        let line = std::mem::take(buf);
                        return Ok(Some(Scan { text: bytes_text(&line), raw: line, source: None, burst: None, decoder: None }));
                    }
                }
                Backend::Hid { dev } => {
                    let mut report = [0u8; 256];
                    let n = dev.read_timeout(&mut report, slice.as_millis() as i32).map_err(|e| format!("HID read failed: {e}"))?;
                    if n > 0 {
                        let r = report[..n].to_vec();
                        return Ok(Some(Scan { text: hid_text(&r), raw: r, source: None, burst: None, decoder: None }));
                    }
                }
                Backend::HidPos { dev, layout } => {
                    let mut report = [0u8; 256];
                    let n = dev.read_timeout(&mut report, slice.as_millis() as i32).map_err(|e| format!("HID read failed: {e}"))?;
                    if n == 0 {
                        continue;
                    }
                    if let Some(scan) = read_hidpos(dev, layout.as_ref(), &report[..n], idle)? {
                        return Ok(Some(scan));
                    }
                }
                Backend::HidKeyboard { devs } => {
                    let per = (slice.as_millis() as i32 / devs.len().max(1) as i32).max(1);
                    let mut done = None;
                    for (dev, id, report, decoder, has_rid) in devs.iter_mut() {
                        let mut buf = [0u8; 64];
                        let n = dev.read_timeout(&mut buf, per).map_err(|e| format!("HID read failed: {e}"))?;
                        let now = Instant::now();
                        if n > 0 {
                            let rid = *has_rid.get_or_insert(n == 9 || (n > 8 && buf[0] != 0));
                            for (usage, down) in report.diff(&buf[..n], rid) {
                                if let Some(b) = decoder.key(usage, down, now) {
                                    done = Some((b, id.clone()));
                                }
                            }
                        }
                        if done.is_none()
                            && let Some(b) = decoder.poll(now, self.idle_ms)
                        {
                            done = Some((b, id.clone()));
                        }
                        if done.is_some() {
                            break;
                        }
                    }
                    if let Some((b, id)) = done
                        && self.accept(&b)
                    {
                        return Ok(Some(Scan { text: b.text.clone(), raw: b.text.as_bytes().to_vec(), source: Some(id), burst: Some(b), decoder: None }));
                    }
                }
                #[cfg(windows)]
                Backend::RawInput { sub, decoders } => {
                    let mut done = None;
                    match sub.rx.recv_timeout(slice) {
                        Ok(k) => {
                            let d = decoders.entry(k.device.to_string()).or_insert_with(|| Decoder::new(self.opts.layout, self.opts.tab_ends));
                            if let Some(b) = d.key(k.usage, k.down, k.at) {
                                done = Some((b, k.device.to_string()));
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return Err("keyboard capture stopped".into()),
                    }
                    if done.is_none() {
                        let now = Instant::now();
                        for (name, d) in decoders.iter_mut() {
                            if let Some(b) = d.poll(now, self.idle_ms) {
                                done = Some((b, name.clone()));
                                break;
                            }
                        }
                    }
                    if let Some((b, name)) = done
                        && self.accept(&b)
                    {
                        let source = crate::rawinput::parse_vid_pid(&name).map(|(v, p)| format!("{v:04x}:{p:04x}")).unwrap_or(name);
                        return Ok(Some(Scan { text: b.text.clone(), raw: b.text.as_bytes().to_vec(), source: Some(source), burst: Some(b), decoder: None }));
                    }
                }
            }
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        match &mut self.backend {
            Backend::Serial { port, .. } => {
                port.write_all(data).map_err(|e| format!("serial write failed: {e}"))?;
                port.flush().map_err(|e| format!("serial write failed: {e}"))?;
                Ok(data.len())
            }
            Backend::HidPos { dev, .. } | Backend::Hid { dev } => dev.write(data).map_err(|e| format!("HID write failed: {e}")),
            _ => Err("cannot send data to a scanner in keyboard mode".into()),
        }
    }

    pub fn info(&self) -> Value {
        json!({
            "mode": self.mode,
            "target": self.target,
            "device": self.device.as_ref().map(|d| d.to_json()),
            "layout": format!("{:?}", self.opts.layout).to_lowercase(),
            "idle_ms": self.idle_ms,
            "min_length": self.min_length,
            "max_gap_ms": self.max_gap_ms,
            "exclusive": self.opts.exclusive && self.mode == "keyboard",
            "blocked": blocked_keys(),
            "hook_stats": hook_stats(),
        })
    }

    /// Leitura -> JSON com a análise do conteúdo.
    pub fn scan_json(&self, s: Scan) -> Value {
        scan_json(s, self.mode, self.opts.aim, self.device.as_ref().map(|d| d.id.clone()))
    }
}

/// Teclas engolidas pelo modo exclusivo desde que a lib carregou.
pub fn blocked_keys() -> u64 {
    #[cfg(windows)]
    return crate::rawinput::BLOCKED.load(std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(windows))]
    0
}

fn hook_stats() -> Value {
    #[cfg(windows)]
    {
        use crate::rawinput::{DELIVERED, DROP, INPUTS, KEYS, PANICS, QUERIES, RECORDED, UNMATCHED};
        use std::sync::atomic::Ordering::Relaxed;
        json!({
            "queries": QUERIES.load(Relaxed), "recorded": RECORDED.load(Relaxed), "unmatched": UNMATCHED.load(Relaxed),
            "inputs": INPUTS.load(Relaxed), "keys": KEYS.load(Relaxed), "delivered": DELIVERED.load(Relaxed), "panics": PANICS.load(Relaxed),
            "drop": [DROP[0].load(Relaxed), DROP[1].load(Relaxed), DROP[2].load(Relaxed), DROP[3].load(Relaxed)],
        })
    }
    #[cfg(not(windows))]
    Value::Null
}

pub fn scan_json(s: Scan, mode: &str, aim: bool, device: Option<String>) -> Value {
    let (prefix, body) = if aim { content::split_aim(&s.text) } else { (None, s.text.as_str()) };
    let symbology = prefix.and_then(content::aim_symbology);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut v = json!({
        "text": body,
        "raw": crate::to_hex(&s.raw),
        "mode": mode,
        "aim": prefix,
        "symbology": symbology,
        "content": content::analyze(body),
        "device": device,
        "source": s.source,
        "time": ms,
    });
    if let Some(d) = s.decoder {
        v["decoder"] = json!(d);
    }
    if let Some(b) = s.burst {
        v["keys"] = json!(b.keys);
        v["avg_gap_ms"] = json!((b.avg_gap_ms * 10.0).round() / 10.0);
        v["terminator"] = json!(b.terminator);
    }
    v
}

/// UTF-8 se for válido; senão Latin-1 (scanners antigos).
pub fn bytes_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

/// Texto imprimível de um relatório HID genérico (até o primeiro NUL,
/// pulando o número do relatório).
fn hid_text(r: &[u8]) -> String {
    let start = usize::from(r.first().is_some_and(|&b| b < 0x20));
    let end = r[start..].iter().position(|&b| b == 0).map(|p| p + start).unwrap_or(r.len());
    bytes_text(&r[start..end])
}

/// Uma leitura HID POS a partir do primeiro relatório: junta as partes
/// seguintes (códigos longos vêm em vários relatórios). Com o layout do
/// descritor, segue o bit de continuação; sem ele, junta o que chegar até
/// `idle` sem relatórios.
fn read_hidpos(dev: &hidapi::HidDevice, layout: Option<&crate::hiddesc::BarcodeLayout>, first: &[u8], idle: Duration) -> Result<Option<Scan>, String> {
    let mut report = [0u8; 256];
    let mut raw = first.to_vec();
    let mut data = Vec::new();
    let mut aim: Option<String> = None;

    if let Some(l) = layout {
        let Some(mut chunk) = l.decode(first) else { return Ok(None) };
        loop {
            data.extend_from_slice(&chunk.data);
            if aim.is_none() && chunk.symbology.first() == Some(&b']') {
                aim = Some(String::from_utf8_lossy(&chunk.symbology).into_owned());
            }
            if !chunk.continued {
                break;
            }
            // Continuação: espera a próxima parte (com folga).
            let wait = idle.max(Duration::from_millis(500)).as_millis() as i32;
            let m = dev.read_timeout(&mut report, wait).map_err(|e| format!("HID read failed: {e}"))?;
            if m == 0 {
                break;
            }
            raw.extend_from_slice(&report[..m]);
            match l.decode(&report[..m]) {
                Some(c) => chunk = c,
                None => break,
            }
        }
    } else {
        let (d, a) = hidpos_chunk(first);
        data.extend_from_slice(&d);
        aim = a;
        loop {
            let m = dev.read_timeout(&mut report, idle.as_millis() as i32).map_err(|e| format!("HID read failed: {e}"))?;
            if m == 0 {
                break;
            }
            raw.extend_from_slice(&report[..m]);
            let (d, a) = hidpos_chunk(&report[..m]);
            data.extend_from_slice(&d);
            aim = aim.or(a);
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    let mut text = bytes_text(&data);
    if let Some(a) = aim
        && content::split_aim(&text).0.is_none()
    {
        text = format!("{a}{text}");
    }
    let decoder = Some(if layout.is_some() { "descriptor" } else { "heuristic" });
    Ok(Some(Scan { text, raw, source: None, burst: None, decoder }))
}

/// Um relatório HID POS sem descritor: `(dados, identificador AIM)`.
///
/// Formato mais comum (Honeywell e compatíveis): byte 0 = relatório 0x02,
/// byte 1 = tamanho, dados, e o identificador de simbologia num campo fixo
/// depois da área de dados. Outros formatos caem no genérico: bytes até o
/// primeiro NUL.
pub fn hidpos_chunk(r: &[u8]) -> (Vec<u8>, Option<String>) {
    if r.len() >= 3 && r[0] == 0x02 && r[1] > 0 && (r[1] as usize) <= r.len() - 2 {
        let len = r[1] as usize;
        let data = r[2..2 + len].to_vec();
        let aim = r
            .windows(3)
            .skip(2 + len)
            .find(|w| w[0] == b']' && w[1].is_ascii_alphabetic() && w[2].is_ascii_alphanumeric())
            .map(|w| String::from_utf8_lossy(w).into_owned());
        return (data, aim);
    }
    let text = hid_text(r);
    (text.into_bytes(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidpos_formats() {
        let mut r = vec![0u8; 64];
        r[0] = 0x02;
        r[1] = 5;
        r[2..7].copy_from_slice(b"12345");
        r[58..61].copy_from_slice(b"]E0");
        let (d, aim) = hidpos_chunk(&r);
        assert_eq!(d, b"12345");
        assert_eq!(aim.as_deref(), Some("]E0"));
        // Genérico
        let (d, aim) = hidpos_chunk(b"\x05hello\0\0\0");
        assert_eq!(d, b"hello");
        assert_eq!(aim, None);
    }

    #[test]
    fn options() {
        let o = Opts::from_json(&json!({"mode": "keyboard", "layout": "abnt2", "terminator": "tab", "max_gap_ms": false})).unwrap();
        assert_eq!(o.layout, Layout::Abnt2);
        assert!(o.tab_ends);
        assert_eq!(o.max_gap_ms, Some(-1.0));
        assert!(Opts::from_json(&json!({"mode": "x"})).is_err());
        assert!(Opts::from_json(&json!({"layout": "dvorak"})).is_err());
    }

    #[test]
    fn selectors() {
        assert!(is_port("COM5"));
        assert!(is_port("com12"));
        assert!(!is_port("COMx"));
        assert!(!is_port("COM"));
        assert!(is_port("/dev/ttyACM0"));
        assert!(is_hid_path(r"\\?\HID#VID_0C2E&PID_0B61#1"));
        assert!(is_hid_path("/dev/hidraw3"));
    }

    #[test]
    fn scan_result_json() {
        let s = Scan { text: "]E07891000315507".into(), raw: b"]E07891000315507".to_vec(), source: None, burst: None, decoder: None };
        let v = scan_json(s, "serial", true, None);
        assert_eq!(v["text"], "7891000315507");
        assert_eq!(v["symbology"], "ean_upc");
        assert_eq!(v["content"]["type"], "ean13");
    }
}
