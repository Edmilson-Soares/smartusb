//! fogusmartusb — reconhecimento de aparelhos USB (leitores de código de
//! barras/QR, smart card, NFC, tokens, biometria, balanças, impressoras...)
//! e leitura de scanners, com API C para o `fogu:ffi`.
//!
//! Convenções (as mesmas do fogusmartcard; o fogu:ffi só tem inteiros e
//! ponteiros, até 8 argumentos, sem callbacks):
//! - Watchers, scanners e aparelhos HID abertos são handles `i64` (> 0).
//! - Funções de status retornam `0` (ou um número >= 0) em sucesso e `-1` em
//!   erro; a mensagem fica em `fsu_last_error`.
//! - Respostas (JSON ou hex) vão para um buffer do chamador, terminadas em
//!   NUL. O retorno é o tamanho escrito, `-(tamanho necessário)` se o buffer
//!   for pequeno, ou -1 em erro. A saída que não coube fica guardada e é
//!   buscada com `fsu_pending`, sem repetir a operação (uma leitura do
//!   scanner nunca se perde por falta de buffer).
//! - Bytes entram e saem em hex.

mod cb;
pub mod classify;
pub mod content;
pub mod enumerate;
pub mod hid;
pub mod hiddesc;
pub mod biometric;
pub mod printer;
pub mod ids;
pub mod keymap;
#[cfg(windows)]
pub mod rawinput;
pub mod scanner;

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use scanner::{Opts, Scanner};

struct Watch {
    include_hubs: bool,
    first: bool,
    ids: HashSet<String>,
    known: HashMap<String, Value>,
    recheck_at: Option<Instant>,
}

static WATCHES: LazyLock<Mutex<HashMap<i64, Arc<Mutex<Watch>>>>> = LazyLock::new(Default::default);
static SCANNERS: LazyLock<Mutex<HashMap<i64, Arc<Mutex<Scanner>>>>> = LazyLock::new(Default::default);
static PRINTERS: LazyLock<Mutex<HashMap<i64, Arc<Mutex<printer::Printer>>>>> = LazyLock::new(Default::default);
static HIDS: LazyLock<Mutex<HashMap<i64, Arc<Mutex<hid::Hid>>>>> = LazyLock::new(Default::default);
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());
static PENDING: Mutex<Option<String>> = Mutex::new(None);

/// Bit de `flags` em `fsu_devices`/`fsu_watch_open`: incluir hubs.
const HUBS: i32 = 1;

fn set_error(msg: impl Into<String>) {
    *LAST_ERROR.lock().unwrap() = msg.into();
}

/// Executa `f` sem deixar um panic atravessar a fronteira C.
fn guard<T>(on_error: T, f: impl FnOnce() -> Result<T, String>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_error(e);
            on_error
        }
        Err(_) => {
            set_error("internal panic in fogusmartusb");
            on_error
        }
    }
}

fn new_handle() -> i64 {
    NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)
}

fn get<T>(map: &Mutex<HashMap<i64, Arc<Mutex<T>>>>, handle: i64, what: &str) -> Result<Arc<Mutex<T>>, String> {
    map.lock().unwrap().get(&handle).cloned().ok_or_else(|| format!("invalid {what} handle {handle}"))
}

fn with<T, R>(map: &Mutex<HashMap<i64, Arc<Mutex<T>>>>, handle: i64, what: &str, f: impl FnOnce(&mut T) -> Result<R, String>) -> Result<R, String> {
    let item = get(map, handle, what)?;
    let mut item = item.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut item)
}

fn close<T>(map: &Mutex<HashMap<i64, Arc<Mutex<T>>>>, handle: i64, what: &str) -> Result<i32, String> {
    map.lock().unwrap().remove(&handle).ok_or_else(|| format!("invalid {what} handle {handle}"))?;
    Ok(0)
}

unsafe fn text<'a>(ptr: *const c_char) -> Result<&'a str, String> {
    if ptr.is_null() {
        return Ok("");
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|_| "string is not valid UTF-8".to_string())
}

unsafe fn json_arg(ptr: *const c_char) -> Result<Value, String> {
    let s = unsafe { text(ptr) }?;
    if s.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(s).map_err(|e| format!("invalid JSON argument: {e}"))
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace() && *c != ':').collect();
    if clean.len() % 2 != 0 {
        return Err("hex string has an odd number of digits".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|_| format!("invalid hex: {s}")))
        .collect()
}

unsafe fn hex_arg(ptr: *const c_char) -> Result<Vec<u8>, String> {
    parse_hex(unsafe { text(ptr) }?)
}

fn timeout_arg(ms: i64) -> Option<Duration> {
    u64::try_from(ms).ok().map(Duration::from_millis)
}

unsafe fn write_out(buf: *mut u8, cap: i64, s: &str) -> i64 {
    if let Some(n) = cb::try_sink(buf, cap, s.as_bytes()) {
        return n;
    }
    let need = s.len() + 1;
    if buf.is_null() || cap < need as i64 {
        *PENDING.lock().unwrap() = Some(s.to_string());
        return -(need as i64);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = 0;
    }
    s.len() as i64
}

unsafe fn write_json(buf: *mut u8, cap: i64, value: Value) -> i64 {
    unsafe { write_out(buf, cap, &value.to_string()) }
}

/// Versão da biblioteca.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_version(buf: *mut u8, cap: i64) -> i64 {
    unsafe { write_out(buf, cap, env!("CARGO_PKG_VERSION")) }
}

/// Saída da última chamada que não coube no buffer. Retorna o tamanho
/// escrito, `-(tamanho)` se ainda não couber, ou -1 se não há nada pendente.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_pending(buf: *mut u8, cap: i64) -> i64 {
    let Some(s) = PENDING.lock().unwrap().take() else {
        set_error("no pending output");
        return -1;
    };
    unsafe { write_out(buf, cap, &s) }
}

/// Última mensagem de erro (0 se não há erro).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_last_error(buf: *mut u8, cap: i64) -> i64 {
    let msg = LAST_ERROR.lock().unwrap().clone();
    unsafe { write_out(buf, cap, &msg) }
}

// ---- aparelhos -----------------------------------------------------------

/// Lista os aparelhos USB conectados, já classificados:
/// `[{ id, vid, pid, name, kind, kinds, category, vendor, product, serial,
/// modes, ports, readers, interfaces, hid, ... }]`. `flags`: 1 = incluir hubs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_devices(flags: i32, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let list: Vec<Value> = enumerate::list(flags & HUBS != 0)?.iter().map(|d| d.to_json()).collect();
        Ok(unsafe { write_json(buf, cap, json!(list)) })
    })
}

/// Um aparelho pelo seletor: id, `"vvvv:pppp"`, `"vvvv:pppp:serial"` ou porta.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_device(selector: *const c_char, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let d = enumerate::find(unsafe { text(selector) }?)?;
        Ok(unsafe { write_json(buf, cap, d.to_json()) })
    })
}

/// Classifica a partir de uma descrição JSON, sem aparelho: `{ "vid",
/// "pid", "product"?, "manufacturer"?, "interfaces"?: [{class, subclass,
/// protocol}], "hid"?: [{usage_page, usage}], "ports"?: [...] }`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_identify(desc_json: *const c_char, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = unsafe { json_arg(desc_json) }?;
        let num = |o: &Value, k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
        let raw = classify::Raw {
            vid: v.get("vid").and_then(ids::parse_id).ok_or("\"vid\" is required")?,
            pid: v.get("pid").and_then(ids::parse_id).unwrap_or(0),
            class: num(&v, "class") as u8,
            product: v.get("product").and_then(Value::as_str).map(str::to_string),
            manufacturer: v.get("manufacturer").and_then(Value::as_str).map(str::to_string),
            interfaces: v
                .get("interfaces")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .map(|i| classify::Iface {
                            number: num(i, "number") as u8,
                            class: num(i, "class") as u8,
                            subclass: num(i, "subclass") as u8,
                            protocol: num(i, "protocol") as u8,
                            name: i.get("name").and_then(Value::as_str).map(str::to_string),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            hid: v
                .get("hid")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .map(|h| classify::HidCol { usage_page: num(h, "usage_page") as u16, usage: num(h, "usage") as u16, ..Default::default() })
                        .collect()
                })
                .unwrap_or_default(),
            ports: v.get("ports").and_then(Value::as_array).map(|l| l.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            ..Default::default()
        };
        let c = classify::classify(&raw);
        Ok(unsafe {
            write_json(
                buf,
                cap,
                json!({
                    "vid": format!("{:04x}", raw.vid),
                    "pid": format!("{:04x}", raw.pid),
                    "name": c.name,
                    "kind": c.kind,
                    "kinds": c.kinds,
                    "category": c.category,
                    "confidence": c.confidence,
                    "source": c.source,
                    "vendor": c.vendor,
                    "product": c.product,
                    "modes": c.modes,
                }),
            )
        })
    })
}

/// Carrega uma base: `.json` (`[{ vid, pid?, name?, vendor?, kind? }]`) ou o
/// `usb.ids`. Retorna quantas entradas entraram ou -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_db_load(path: *const c_char) -> i64 {
    guard(-1, || Ok(ids::load_file(unsafe { text(path) }?)? as i64))
}

#[unsafe(no_mangle)]
pub extern "C" fn fsu_db_clear() -> i32 {
    ids::clear();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn fsu_db_count() -> i64 {
    ids::count() as i64
}

/// Leitores PC/SC presentes (nomes), para usar com o fogusmartcard.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_pcsc_readers(buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || Ok(unsafe { write_json(buf, cap, json!(enumerate::pcsc_readers())) }))
}

// ---- eventos (conectar/desconectar) ---------------------------------------

impl Watch {
    fn snapshot(&self) -> Result<HashMap<String, Value>, String> {
        Ok(enumerate::list(self.include_hubs)?.into_iter().map(|d| (d.id.clone(), d.to_json())).collect())
    }

    fn events(&mut self, new: HashMap<String, Value>) -> Vec<Value> {
        let ev = |event: &str, d: &Value| json!({ "event": event, "id": d["id"], "kind": d["kind"], "name": d["name"], "device": d });
        let (added, removed) = enumerate::diff(&self.known, &new);
        let mut out: Vec<Value> = removed.into_iter().map(|d| ev("removed", d)).collect();
        out.extend(added.into_iter().map(|d| ev("added", d)));
        let mut changed: Vec<&Value> = new.iter().filter(|(k, v)| self.known.get(*k).is_some_and(|old| old != *v)).map(|(_, v)| v).collect();
        changed.sort_by_key(|v| v["id"].as_str().unwrap_or("").to_string());
        out.extend(changed.into_iter().map(|d| ev("changed", d)));
        self.ids = new.keys().cloned().collect();
        self.known = new;
        out
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Result<Vec<Value>, String> {
        let deadline = timeout.map(|t| Instant::now() + t);
        if self.first {
            self.first = false;
            let snap = self.snapshot()?;
            let events = self.events(snap);
            if !events.is_empty() {
                return Ok(events);
            }
        }
        loop {
            let ids = enumerate::quick_ids(self.include_hubs)?;
            let recheck = self.recheck_at.is_some_and(|t| Instant::now() >= t);
            if ids != self.ids || recheck {
                if ids != self.ids {
                    // Espera o driver subir (porta COM, leitor PC/SC, HID).
                    std::thread::sleep(Duration::from_millis(500));
                    self.recheck_at = Some(Instant::now() + Duration::from_millis(1500));
                } else {
                    self.recheck_at = None;
                }
                let snap = self.snapshot()?;
                let events = self.events(snap);
                if !events.is_empty() {
                    return Ok(events);
                }
            }
            let now = Instant::now();
            if deadline.is_some_and(|d| now >= d) {
                return Ok(Vec::new());
            }
            let step = Duration::from_millis(250);
            std::thread::sleep(deadline.map_or(step, |d| step.min(d.saturating_duration_since(now))));
        }
    }
}

/// Abre um observador de conexões USB. `flags`: 1 = incluir hubs.
#[unsafe(no_mangle)]
pub extern "C" fn fsu_watch_open(flags: i32) -> i64 {
    guard(-1, || {
        let w = Watch { include_hubs: flags & HUBS != 0, first: true, ids: HashSet::new(), known: HashMap::new(), recheck_at: None };
        let h = new_handle();
        WATCHES.lock().unwrap().insert(h, Arc::new(Mutex::new(w)));
        Ok(h)
    })
}

/// Espera até `timeout_ms` (< 0 = sem limite) e escreve `[{ event: "added"
/// | "removed" | "changed", id, kind, name, device }]`; `[]` se o tempo
/// acabou. Na primeira chamada os aparelhos presentes vêm como `added`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_watch_wait(watch: i64, timeout_ms: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let events = with(&WATCHES, watch, "watch", |w| w.wait(timeout_arg(timeout_ms)))?;
        Ok(unsafe { write_json(buf, cap, json!(events)) })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fsu_watch_close(watch: i64) -> i32 {
    guard(-1, || close(&WATCHES, watch, "watch"))
}

// ---- scanners --------------------------------------------------------------

/// Abre um scanner. `selector`: vazio (primeiro scanner reconhecido), id,
/// `vvvv:pppp`, porta (`COM5`, `/dev/ttyACM0`), caminho HID, ou `keyboard`
/// (qualquer teclado, com filtro de rajada). `opts_json`: `{ mode, layout,
/// idle_ms, terminator, min_length, max_gap_ms, baud, port, usage_page, aim }`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_scanner_open(selector: *const c_char, opts_json: *const c_char) -> i64 {
    guard(-1, || {
        let sel = unsafe { text(selector) }?;
        let opts = Opts::from_json(&unsafe { json_arg(opts_json) }?)?;
        let s = Scanner::open(sel, opts)?;
        let h = new_handle();
        SCANNERS.lock().unwrap().insert(h, Arc::new(Mutex::new(s)));
        Ok(h)
    })
}

/// Espera uma leitura por até `timeout_ms` (< 0 = sem limite). Escreve
/// `{ text, raw, mode, aim, symbology, content, device, source, time }` ou
/// `null` se o tempo acabou.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_scanner_read(scanner: i64, timeout_ms: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = with(&SCANNERS, scanner, "scanner", |s| Ok(s.read(timeout_arg(timeout_ms))?.map(|scan| s.scan_json(scan))))?;
        Ok(unsafe { write_json(buf, cap, v.unwrap_or(Value::Null)) })
    })
}

/// Envia bytes (hex) ao scanner (comandos em modo serial ou HID). Retorna
/// quantos bytes foram enviados.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_scanner_write(scanner: i64, data_hex: *const c_char) -> i64 {
    guard(-1, || {
        let data = unsafe { hex_arg(data_hex) }?;
        Ok(with(&SCANNERS, scanner, "scanner", |s| s.write(&data))? as i64)
    })
}

/// `{ mode, target, device, layout, idle_ms, min_length, max_gap_ms }`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_scanner_info(scanner: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = with(&SCANNERS, scanner, "scanner", |s| Ok(s.info()))?;
        Ok(unsafe { write_json(buf, cap, v) })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fsu_scanner_close(scanner: i64) -> i32 {
    guard(-1, || close(&SCANNERS, scanner, "scanner"))
}

/// Analisa um texto lido (sem scanner): identificador AIM e tipo do
/// conteúdo (URL, PIX, NF-e, boleto, EAN, CPF/CNPJ, Wi-Fi, vCard...).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_analyze(text_in: *const c_char, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let t = unsafe { text(text_in) }?;
        let scan = scanner::Scan { text: t.to_string(), raw: t.as_bytes().to_vec(), source: None, burst: None, decoder: None };
        let mut v = scanner::scan_json(scan, "text", true, None);
        if let Some(o) = v.as_object_mut() {
            for k in ["mode", "device", "source", "time"] {
                o.remove(k);
            }
        }
        Ok(unsafe { write_json(buf, cap, v) })
    })
}

// ---- HID genérico -----------------------------------------------------------

/// Abre um aparelho HID. `selector`: caminho HID, id ou `vvvv:pppp`.
/// `opts_json`: `{ usage_page, usage, interface }` para escolher a coleção.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_hid_open(selector: *const c_char, opts_json: *const c_char) -> i64 {
    guard(-1, || {
        let sel = unsafe { text(selector) }?;
        let opts = unsafe { json_arg(opts_json) }?;
        let h = hid::open(sel, &opts)?;
        let handle = new_handle();
        HIDS.lock().unwrap().insert(handle, Arc::new(Mutex::new(h)));
        Ok(handle)
    })
}

/// Lê um relatório de entrada (hex). Escreve `""` se o tempo acabou.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_hid_read(hid: i64, timeout_ms: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let t = i32::try_from(timeout_ms.max(-1)).unwrap_or(i32::MAX);
        let data = with(&HIDS, hid, "HID", |h| {
            let mut b = [0u8; 4096];
            let n = h.dev.read_timeout(&mut b, t).map_err(|e| format!("HID read failed: {e}"))?;
            Ok(b[..n].to_vec())
        })?;
        Ok(unsafe { write_out(buf, cap, &to_hex(&data)) })
    })
}

/// Envia um relatório de saída (hex; o 1º byte é o número do relatório, 00
/// se o aparelho não usa números). Retorna quantos bytes foram enviados.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_hid_write(hid: i64, data_hex: *const c_char) -> i64 {
    guard(-1, || {
        let data = unsafe { hex_arg(data_hex) }?;
        Ok(with(&HIDS, hid, "HID", |h| h.dev.write(&data).map_err(|e| format!("HID write failed: {e}")))? as i64)
    })
}

/// Lê um relatório de feature (`report_id`, até `len` bytes) em hex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_hid_get_feature(hid: i64, report_id: i32, len: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let id = u8::try_from(report_id).map_err(|_| "report id must be 0..=255".to_string())?;
        let len = usize::try_from(len).ok().filter(|&l| (1..=4096).contains(&l)).ok_or("length must be 1..=4096")?;
        let data = with(&HIDS, hid, "HID", |h| {
            let mut b = vec![0u8; len + 1];
            b[0] = id;
            let n = h.dev.get_feature_report(&mut b).map_err(|e| format!("HID get feature failed: {e}"))?;
            b.truncate(n);
            Ok(b)
        })?;
        Ok(unsafe { write_out(buf, cap, &to_hex(&data)) })
    })
}

/// Envia um relatório de feature (hex, com o número do relatório no 1º byte).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_hid_send_feature(hid: i64, data_hex: *const c_char) -> i32 {
    guard(-1, || {
        let data = unsafe { hex_arg(data_hex) }?;
        with(&HIDS, hid, "HID", |h| h.dev.send_feature_report(&data).map_err(|e| format!("HID send feature failed: {e}")))?;
        Ok(0)
    })
}

/// `{ path, usage_page, usage, vid, pid, product, manufacturer, serial }`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_hid_info(hid: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = with(&HIDS, hid, "HID", |h| Ok(h.info()))?;
        Ok(unsafe { write_json(buf, cap, v) })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fsu_hid_close(hid: i64) -> i32 {
    guard(-1, || close(&HIDS, hid, "HID"))
}

// ---- impressoras ------------------------------------------------------------

/// `{ usb: [aparelhos do tipo printer], system: [{ name, port, driver,
/// default }] (Windows), usbprint: [caminhos] (Windows) }`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_printers(buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let usb: Vec<Value> = enumerate::list(false)?.iter().filter(|d| d.class.kinds.iter().any(|k| k == "printer")).map(|d| d.to_json()).collect();
        let v = json!({ "usb": usb, "system": printer::system_printers(), "usbprint": printer::usbprint_paths() });
        Ok(unsafe { write_json(buf, cap, v) })
    })
}

/// Abre uma impressora. `selector`: `""` (primeira USB), id, `vvvv:pppp`,
/// `"printer:<nome instalado>"`, porta (`COM3`), `/dev/usb/lp0` ou caminho.
/// `opts_json`: `{ backend: "auto" | "spooler" | "usbprint" | "usb" |
/// "serial" | "file", baud, timeout_ms }`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_printer_open(selector: *const c_char, opts_json: *const c_char) -> i64 {
    guard(-1, || {
        let sel = unsafe { text(selector) }?;
        let opts = unsafe { json_arg(opts_json) }?;
        let p = printer::Printer::open(sel, &opts)?;
        let h = new_handle();
        PRINTERS.lock().unwrap().insert(h, Arc::new(Mutex::new(p)));
        Ok(h)
    })
}

/// Envia bytes (hex). No spooler, cada chamada é um trabalho de impressão.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_printer_write(printer: i64, data_hex: *const c_char) -> i64 {
    guard(-1, || {
        let data = unsafe { hex_arg(data_hex) }?;
        Ok(with(&PRINTERS, printer, "printer", |p| p.write(&data))? as i64)
    })
}

/// Lê a resposta da impressora (hex; `""` se o tempo acabar). Só nos
/// caminhos `usb` e `serial`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_printer_read(printer: i64, timeout_ms: i64, max: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let t = Duration::from_millis(timeout_ms.max(0) as u64);
        let data = with(&PRINTERS, printer, "printer", |p| p.read(t, max.max(1) as usize))?;
        Ok(unsafe { write_out(buf, cap, &to_hex(&data)) })
    })
}

/// `{ backend, target, device }`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_printer_info(printer: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = with(&PRINTERS, printer, "printer", |p| Ok(p.info()))?;
        Ok(unsafe { write_json(buf, cap, v) })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fsu_printer_close(printer: i64) -> i32 {
    guard(-1, || close(&PRINTERS, printer, "printer"))
}

/// Gera bytes ESC/POS (hex) de uma lista JSON de comandos (ver printer::escpos).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_escpos(cmds_json: *const c_char, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = unsafe { json_arg(cmds_json) }?;
        let bytes = printer::escpos(&v)?;
        Ok(unsafe { write_out(buf, cap, &to_hex(&bytes)) })
    })
}

/// Texto na página de código da impressora (cp437, cp850, cp860, cp1252,
/// latin1, utf8), em hex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_encode(text_in: *const c_char, codepage: *const c_char, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let bytes = printer::encode(unsafe { text(text_in) }?, unsafe { text(codepage) }?)?;
        Ok(unsafe { write_out(buf, cap, &to_hex(&bytes)) })
    })
}

// ---- biometria (Windows Biometric Framework) ---------------------------------

/// Leitores de digital: `[{ unit_id, description, manufacturer, model,
/// serial, device_instance_id, pool, firmware }]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_bio_units(buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || Ok(unsafe { write_json(buf, cap, json!(biometric::units()?)) }))
}

/// Espera um dedo e identifica pelo Windows Hello: `{ matched, account,
/// sid, finger, unit_id }`, `{ matched: false }` ou `null` (tempo acabou).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_bio_identify(timeout_ms: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = biometric::identify(timeout_arg(timeout_ms))?;
        Ok(unsafe { write_json(buf, cap, v.unwrap_or(Value::Null)) })
    })
}

/// Captura a imagem da digital (precisa de administrador): `{ width,
/// height, depth, dpi, finger, quality, record, saved }` ou `null`. `save`:
/// caminho .pgm/.bmp (vazio = não salva).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsu_bio_capture(timeout_ms: i64, save: *const c_char, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let v = biometric::capture(timeout_arg(timeout_ms), unsafe { text(save) }?)?;
        Ok(unsafe { write_json(buf, cap, v.unwrap_or(Value::Null)) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// LAST_ERROR e PENDING são globais: testes que os conferem não rodam juntos.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn call(f: impl FnOnce(*mut u8, i64) -> i64) -> (i64, String) {
        let mut buf = vec![0u8; 1 << 20];
        let n = f(buf.as_mut_ptr(), buf.len() as i64);
        let s = if n >= 0 { String::from_utf8(buf[..n as usize].to_vec()).unwrap() } else { String::new() };
        (n, s)
    }

    #[test]
    fn version() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (n, v) = call(|b, c| unsafe { fsu_version(b, c) });
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
        assert_eq!(n as usize, v.len());
    }

    #[test]
    fn analyze_over_ffi() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let t = CString::new("]Q1https://example.com").unwrap();
        let (_, j) = call(|b, c| unsafe { fsu_analyze(t.as_ptr(), b, c) });
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["text"], "https://example.com");
        assert_eq!(v["symbology"], "qrcode");
        assert_eq!(v["content"]["type"], "url");
    }

    #[test]
    fn identify_over_ffi() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = CString::new(r#"{"vid":"0c2e","pid":"0b61","hid":[{"usage_page":1,"usage":6}]}"#).unwrap();
        let (_, j) = call(|b, c| unsafe { fsu_identify(d.as_ptr(), b, c) });
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["kind"], "barcode_scanner");
        let bad = CString::new("{}").unwrap();
        assert_eq!(call(|b, c| unsafe { fsu_identify(bad.as_ptr(), b, c) }).0, -1);
        let (_, e) = call(|b, c| unsafe { fsu_last_error(b, c) });
        assert!(e.contains("vid"));
    }

    #[test]
    fn pending_output_is_kept() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut tiny = [0u8; 2];
        let v = env!("CARGO_PKG_VERSION");
        assert_eq!(unsafe { fsu_version(tiny.as_mut_ptr(), 2) }, -(v.len() as i64 + 1));
        assert_eq!(call(|b, c| unsafe { fsu_pending(b, c) }).1, v);
        assert_eq!(call(|b, c| unsafe { fsu_pending(b, c) }).0, -1);
    }

    #[test]
    fn devices_and_watch() {
        let (n, j) = call(|b, c| unsafe { fsu_devices(0, b, c) });
        assert!(n > 0, "fsu_devices failed");
        let list: Value = serde_json::from_str(&j).unwrap();
        assert!(list.is_array());
        let w = fsu_watch_open(0);
        let (_, j) = call(|b, c| unsafe { fsu_watch_wait(w, 100, b, c) });
        let ev: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(ev.as_array().unwrap().len(), list.as_array().unwrap().len());
        let (_, j) = call(|b, c| unsafe { fsu_watch_wait(w, 100, b, c) });
        assert_eq!(j, "[]");
        assert_eq!(fsu_watch_close(w), 0);
        assert_eq!(fsu_watch_close(w), -1);
    }

    #[test]
    fn printer_helpers_over_ffi() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let cmds = CString::new(r#"[{"init":true},{"codepage":"cp860"},{"line":"Ação"},{"cut":true}]"#).unwrap();
        let (_, h) = call(|b, c| unsafe { fsu_escpos(cmds.as_ptr(), b, c) });
        assert_eq!(h, "1b401b74034187846f0a1d564103");
        let t = CString::new("ç").unwrap();
        let cp = CString::new("850").unwrap();
        assert_eq!(call(|b, c| unsafe { fsu_encode(t.as_ptr(), cp.as_ptr(), b, c) }).1, "87");
        let (n, j) = call(|b, c| unsafe { fsu_printers(b, c) });
        assert!(n > 0);
        let v: Value = serde_json::from_str(&j).unwrap();
        assert!(v["system"].is_array());
        let none = CString::new("printer:__no_such_printer__").unwrap();
        assert_eq!(unsafe { fsu_printer_open(none.as_ptr(), std::ptr::null()) }, -1);
    }

    #[test]
    fn invalid_handles_and_args() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(fsu_scanner_close(999_999), -1);
        assert_eq!(fsu_hid_close(999_999), -1);
        assert_eq!(call(|b, c| unsafe { fsu_scanner_read(999_999, 0, b, c) }).0, -1);
        let bad = CString::new(r#"{"mode":"nope"}"#).unwrap();
        let sel = CString::new("COM250").unwrap();
        assert_eq!(unsafe { fsu_scanner_open(sel.as_ptr(), bad.as_ptr()) }, -1);
        assert_eq!(parse_hex("0a ff:10").unwrap(), vec![10, 255, 16]);
        assert!(parse_hex("abc").is_err());
    }
}

/// Registra (ou remove, com NULL) o callback (Fogu 0.7+, `ffi.callback`) que
/// recebe as saídas das funções chamadas com `buf = NULL, cap = 0`: a resposta
/// chega inteira de uma vez, sem buffer fixo nem `fsu_pending` (ver `cb.rs`).
#[unsafe(no_mangle)]
pub extern "C" fn fsu_set_sink(sink: Option<cb::Sink>) -> i32 {
    cb::set_sink(sink);
    0
}
