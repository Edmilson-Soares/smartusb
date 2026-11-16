//! Captura de teclados no Windows por Raw Input. O Windows não deixa abrir
//! teclados pelo HID (o sistema os tem em uso exclusivo), mas o Raw Input
//! entrega as teclas de cada teclado físico separadamente, com o aparelho de
//! origem, mesmo com o programa em segundo plano. Assim dá para ler um
//! scanner em modo teclado sem confundir com o teclado de verdade.
//!
//! Uma thread com uma janela só de mensagens recebe `WM_INPUT` e repassa os
//! eventos aos assinantes. O Windows só permite um destino de Raw Input por
//! processo para teclados, então a thread é única e compartilhada.
//!
//! ## Modo exclusivo
//!
//! O Raw Input só observa: as teclas continuam indo para a janela em foco.
//! Para bloqueá-las, um hook global `WH_KEYBOARD` (esta DLL é carregada
//! pelo Windows nos programas com janela) pergunta à nossa janela, para cada
//! tecla que o programa em foco vai receber, se ela veio do scanner. O
//! `WM_INPUT` dessa tecla chega antes do hook (a mesma técnica de "bloquear
//! um teclado específico" do Raw Input + hook), então dá para decidir pelo
//! aparelho de origem. Teclas do scanner são engolidas; as do teclado de
//! verdade passam normalmente.
//!
//! Limites do Windows: o hook só entra em programas de 64 bits da mesma
//! sessão e sem privilégio maior (UIPI); programas de 32 bits, elevados ou
//! que leem o teclado por Raw Input/DirectInput ainda veem as teclas.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK,
    RIDI_DEVICENAME, RIM_TYPEKEYBOARD, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, GetMessageW, HHOOK, HWND_MESSAGE, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, SendMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD,
    WM_APP, WM_INPUT, WNDCLASSW,
};

use crate::keymap::set1_to_usage;

const RI_KEY_BREAK: u16 = 1;
const RI_KEY_E0: u16 = 2;
const RI_KEY_E1: u16 = 4;

const CLASS: &str = "fogusmartusb_rawinput";
/// Hook -> janela: "bloqueio esta tecla?" (wParam = vk, lParam = flags da
/// tecla). Só no HC_ACTION (quando o programa retira a tecla da fila): no
/// HC_NOREMOVE (uma espiada), devolver "bloquear" faz o Windows descartar a
/// mensagem sem passar pelo HC_ACTION, e a decisão ficaria sem registro.
const WM_DECIDE: u32 = WM_APP + 0x51;
/// Pedido interno para instalar (wParam = 1) ou remover (0) o hook.
const WM_HOOK: u32 = WM_APP + 0x53;

#[derive(Clone, Debug)]
pub struct KeyMsg {
    /// Nome do aparelho (`\\?\HID#VID_0C2E&PID_0B61&MI_00#...`), ou
    /// `"synthetic"` para teclas injetadas por software.
    pub device: Arc<str>,
    pub vid: u16,
    pub pid: u16,
    pub usage: u8,
    pub down: bool,
    pub at: Instant,
}

struct Sub {
    id: u64,
    filter: Option<(u16, u16)>,
    synthetic: bool,
    exclusive: bool,
    tx: Sender<KeyMsg>,
}

/// Tecla vista pelo Raw Input, esperando a decisão do hook.
struct Recent {
    make: u16,
    e0: bool,
    down: bool,
    block: bool,
    at: Instant,
}

static SUBS: Mutex<Vec<Sub>> = Mutex::new(Vec::new());
static RECENT: Mutex<VecDeque<Recent>> = Mutex::new(VecDeque::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
/// Teclas engolidas pelo modo exclusivo (para diagnóstico).
pub static BLOCKED: AtomicU64 = AtomicU64::new(0);
/// Diagnóstico: perguntas do hook e teclas registradas para decisão.
pub static QUERIES: AtomicU64 = AtomicU64::new(0);
pub static RECORDED: AtomicU64 = AtomicU64::new(0);
pub static UNMATCHED: AtomicU64 = AtomicU64::new(0);
/// Diagnóstico (info().hook_stats): WM_INPUT recebidos, teclas lidas, teclas
/// entregues a assinantes, panics em on_input e descartes por etapa
/// (DROP: tamanho, leitura, não é teclado, filtro E1/MakeCode/VKey). Se
/// `inputs` cresce e `keys` fica em 0, o Windows entregou os eventos com
/// MakeCode/VKey zerados (acontece com teclas injetadas por software e em
/// alguns cenários de proteção do Windows 11).
pub static INPUTS: AtomicU64 = AtomicU64::new(0);
pub static KEYS: AtomicU64 = AtomicU64::new(0);
pub static DELIVERED: AtomicU64 = AtomicU64::new(0);
pub static PANICS: AtomicU64 = AtomicU64::new(0);
pub static DROP: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Assinatura: recebe as teclas enquanto existir.
pub struct Subscription {
    id: u64,
    exclusive: bool,
    pub rx: Receiver<KeyMsg>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let any_exclusive = {
            let mut subs = SUBS.lock().unwrap();
            subs.retain(|s| s.id != self.id);
            subs.iter().any(|s| s.exclusive)
        };
        if self.exclusive && !any_exclusive {
            let _ = set_hook(false);
        }
    }
}

/// `filter`: só teclas desse VID/PID (None = todos os teclados).
/// `synthetic`: aceita também teclas injetadas por software (testes).
/// `exclusive`: as teclas do aparelho (`filter`) não chegam aos programas.
pub fn subscribe(filter: Option<(u16, u16)>, synthetic: bool, exclusive: bool) -> Result<Subscription, String> {
    if exclusive && filter.is_none() && !synthetic {
        return Err("exclusive mode needs a specific device (it would block the real keyboard too)".into());
    }
    start()?;
    if exclusive {
        set_hook(true)?;
    }
    let (tx, rx) = channel();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    SUBS.lock().unwrap().push(Sub { id, filter, synthetic, exclusive, tx });
    Ok(Subscription { id, exclusive, rx })
}

fn start() -> Result<(), String> {
    STARTED
        .get_or_init(|| {
            let (tx, rx) = channel();
            std::thread::Builder::new()
                .name("fogusmartusb-rawinput".into())
                .spawn(move || unsafe { run(tx) })
                .map_err(|e| format!("cannot start keyboard capture thread: {e}"))?;
            rx.recv().unwrap_or_else(|_| Err("keyboard capture thread exited".into()))
        })
        .clone()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Instala/remove o hook pela thread da janela (que vive enquanto o
/// processo viver; um hook global morre com a thread que o instalou).
fn set_hook(on: bool) -> Result<(), String> {
    let hwnd = HWND_MAIN.load(Ordering::Acquire) as HWND;
    if hwnd.is_null() {
        return Err("keyboard capture is not running".into());
    }
    let r = unsafe { SendMessageW(hwnd, WM_HOOK, on as usize, 0) };
    if r == 0 { Ok(()) } else { Err(HOOK_ERROR.lock().unwrap().clone()) }
}

static HOOK_ERROR: Mutex<String> = Mutex::new(String::new());

/// Módulo desta biblioteca (para o hook global, que precisa de uma DLL).
unsafe fn this_module() -> Result<HMODULE, String> {
    unsafe {
        let mut hmod: HMODULE = std::ptr::null_mut();
        let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        if GetModuleHandleExW(flags, keyboard_hook as *const u16, &mut hmod) == 0 {
            return Err(format!("GetModuleHandleExW failed: {}", std::io::Error::last_os_error()));
        }
        let mut name = [0u16; 1024];
        let n = GetModuleFileNameW(hmod, name.as_mut_ptr(), name.len() as u32) as usize;
        let path = String::from_utf16_lossy(&name[..n]).to_ascii_lowercase();
        if !path.ends_with(".dll") {
            return Err("exclusive mode needs fogusmartusb as a DLL (it is loaded into other programs by the keyboard hook)".into());
        }
        Ok(hmod)
    }
}

unsafe fn hook_on_thread(on: bool) -> Result<(), String> {
    unsafe {
        let current = HOOK.load(Ordering::Acquire);
        if on {
            if current != 0 {
                return Ok(());
            }
            let hmod = this_module()?;
            let h = SetWindowsHookExW(WH_KEYBOARD, Some(keyboard_hook), hmod, 0);
            if h.is_null() {
                return Err(format!("SetWindowsHookExW failed: {}", std::io::Error::last_os_error()));
            }
            HOOK.store(h as isize, Ordering::Release);
        } else if current != 0 {
            UnhookWindowsHookEx(current as HHOOK);
            HOOK.store(0, Ordering::Release);
        }
        Ok(())
    }
}

unsafe fn run(ready: Sender<Result<(), String>>) {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide(CLASS);
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), hinst, std::ptr::null());
        if hwnd.is_null() {
            let _ = ready.send(Err(format!("CreateWindowExW failed: {}", std::io::Error::last_os_error())));
            return;
        }
        let rid = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        if RegisterRawInputDevices(&rid, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            let _ = ready.send(Err(format!("RegisterRawInputDevices failed: {}", std::io::Error::last_os_error())));
            return;
        }
        HWND_MAIN.store(hwnd as isize, Ordering::Release);
        let _ = ready.send(Ok(()));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            let at = Instant::now();
            INPUTS.fetch_add(1, Ordering::Relaxed);
            // Um panic não pode atravessar o callback do Windows.
            if std::panic::catch_unwind(|| unsafe { on_input(lparam as HRAWINPUT, at) }).is_err() {
                PANICS.fetch_add(1, Ordering::Relaxed);
            }
        }
        WM_DECIDE => {
            // Mensagens enviadas são atendidas antes das postadas: processa
            // primeiro o Raw Input pendente, que é o da tecla em questão.
            unsafe {
                let mut m: MSG = std::mem::zeroed();
                while PeekMessageW(&mut m, hwnd, WM_INPUT, WM_INPUT, PM_REMOVE) != 0 {
                    DispatchMessageW(&m);
                }
            }
            return std::panic::catch_unwind(|| decide(lparam)).unwrap_or(0);
        }
        WM_HOOK => {
            let r = unsafe { hook_on_thread(wparam != 0) };
            return match r {
                Ok(()) => 0,
                Err(e) => {
                    *HOOK_ERROR.lock().unwrap() = e;
                    1
                }
            };
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Decide pela tecla mais antiga vista pelo Raw Input com o mesmo scancode
/// e direção. 1 = bloquear.
fn decide(lparam: LPARAM) -> LRESULT {
    QUERIES.fetch_add(1, Ordering::Relaxed);
    let flags = lparam as u32;
    let make = ((flags >> 16) & 0xFF) as u16;
    let e0 = flags & (1 << 24) != 0;
    let down = flags & (1 << 31) == 0;
    let mut recent = RECENT.lock().unwrap();
    let now = Instant::now();
    recent.retain(|r| now.duration_since(r.at) < Duration::from_secs(2));
    let Some(i) = recent.iter().position(|r| r.make == make && r.e0 == e0 && r.down == down) else {
        UNMATCHED.fetch_add(1, Ordering::Relaxed);
        return 0;
    };
    let block = recent.remove(i).is_some_and(|r| r.block);
    if block {
        BLOCKED.fetch_add(1, Ordering::Relaxed);
    }
    block as LRESULT
}

/// O hook roda dentro do programa em foco (esta DLL carregada lá) e pergunta
/// a cada janela de captura (uma por processo que usa a lib).
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    const HC_ACTION: i32 = 0;
    unsafe {
        if code == HC_ACTION {
            let class = wide(CLASS);
            let msg = WM_DECIDE;
            let mut after: HWND = std::ptr::null_mut();
            loop {
                let w = FindWindowExW(HWND_MESSAGE, after, class.as_ptr(), std::ptr::null());
                if w.is_null() {
                    break;
                }
                let mut result: usize = 0;
                if SendMessageTimeoutW(w, msg, wparam, lparam, SMTO_ABORTIFHUNG, 100, &mut result) != 0 && result == 1 {
                    return 1;
                }
                after = w;
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }
}

thread_local! {
    static NAMES: std::cell::RefCell<HashMap<usize, (Arc<str>, u16, u16)>> = Default::default();
}

unsafe fn device_info(h: HANDLE) -> (Arc<str>, u16, u16) {
    NAMES.with(|names| {
        if let Some(v) = names.borrow().get(&(h as usize)) {
            return v.clone();
        }
        let mut len: u32 = 0;
        let name = unsafe {
            GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, std::ptr::null_mut(), &mut len);
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, buf.as_mut_ptr().cast(), &mut len);
            if n == u32::MAX { String::new() } else { String::from_utf16_lossy(&buf[..n as usize]).trim_end_matches('\0').to_string() }
        };
        let (vid, pid) = parse_vid_pid(&name).unwrap_or((0, 0));
        let v: (Arc<str>, u16, u16) = (Arc::from(name.as_str()), vid, pid);
        names.borrow_mut().insert(h as usize, v.clone());
        v
    })
}

/// `...VID_0C2E&PID_0B61...` -> (0x0C2E, 0x0B61)
pub fn parse_vid_pid(name: &str) -> Option<(u16, u16)> {
    let up = name.to_ascii_uppercase();
    let hex = |key: &str| {
        let i = up.find(key)? + key.len();
        u16::from_str_radix(up.get(i..i + 4)?, 16).ok()
    };
    Some((hex("VID_")?, hex("PID_")?))
}

unsafe fn on_input(h: HRAWINPUT, at: Instant) {
    let header = size_of::<RAWINPUTHEADER>() as u32;
    let mut size: u32 = 0;
    unsafe {
        if GetRawInputData(h, RID_INPUT, std::ptr::null_mut(), &mut size, header) != 0 || size == 0 {
            DROP[0].fetch_add(1, Ordering::Relaxed);
            return;
        }
        // u64 para alinhar a struct.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        if GetRawInputData(h, RID_INPUT, buf.as_mut_ptr().cast(), &mut size, header) == u32::MAX {
            DROP[1].fetch_add(1, Ordering::Relaxed);
            return;
        }
        let raw = &*(buf.as_ptr() as *const RAWINPUT);
        if raw.header.dwType != RIM_TYPEKEYBOARD {
            DROP[2].fetch_add(1, Ordering::Relaxed);
            return;
        }
        let kb = raw.data.keyboard;
        if kb.Flags & RI_KEY_E1 != 0 || kb.MakeCode == 0 || kb.MakeCode == 0xFF || kb.VKey == 0xFF {
            DROP[3].fetch_add(1, Ordering::Relaxed);
            return;
        }
        let e0 = kb.Flags & RI_KEY_E0 != 0;
        let down = kb.Flags & RI_KEY_BREAK == 0;
        KEYS.fetch_add(1, Ordering::Relaxed);
        let synthetic = raw.header.hDevice.is_null();
        let (device, vid, pid) = if synthetic { (Arc::from("synthetic"), 0, 0) } else { device_info(raw.header.hDevice) };
        let wanted = |s: &Sub| if synthetic { s.synthetic } else { s.filter.is_none_or(|f| f == (vid, pid)) };

        let mut subs = SUBS.lock().unwrap();
        // Registro para o hook decidir; só quando há assinante exclusivo.
        if subs.iter().any(|s| s.exclusive) {
            let block = subs.iter().any(|s| s.exclusive && wanted(s) && (synthetic || s.filter.is_some()));
            let mut recent = RECENT.lock().unwrap();
            recent.push_back(Recent { make: kb.MakeCode, e0, down, block, at });
            RECORDED.fetch_add(1, Ordering::Relaxed);
            while recent.len() > 512 {
                recent.pop_front();
            }
        }
        let Some(usage) = set1_to_usage(kb.MakeCode, e0) else { return };
        let msg = KeyMsg { device, vid, pid, usage, down, at };
        let before = subs.len();
        subs.retain(|s| !wanted(s) || s.tx.send(msg.clone()).is_ok());
        if subs.iter().any(|s| wanted(s)) || before != subs.len() {
            DELIVERED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vid_pid_from_name() {
        assert_eq!(parse_vid_pid(r"\\?\HID#VID_0C2E&PID_0B61&MI_00#7&2d3#{884b96c3}"), Some((0x0C2E, 0x0B61)));
        assert_eq!(parse_vid_pid(r"\\?\ACPI#PNP0303#4&1"), None);
    }

    /// A janela de mensagens e o registro do Raw Input funcionam nesta máquina.
    #[test]
    fn capture_starts() {
        let a = subscribe(None, false, false).expect("raw input");
        let b = subscribe(Some((0x0C2E, 0x0B61)), false, false).expect("second subscriber");
        let a_id = a.id;
        assert_eq!(SUBS.lock().unwrap().iter().filter(|s| s.id == a_id || s.id == b.id).count(), 2);
        drop(a);
        assert!(SUBS.lock().unwrap().iter().all(|s| s.id != a_id));
    }

    #[test]
    fn exclusive_rules() {
        // Sem aparelho, bloquearia o teclado de verdade.
        assert!(subscribe(None, false, true).is_err());
        // No executável de teste (não é DLL) o hook global é recusado.
        let e = subscribe(Some((1, 2)), false, true).err().unwrap();
        assert!(e.contains("DLL"), "{e}");
    }

    #[test]
    fn decide_matches_scancode_and_direction() {
        let now = Instant::now();
        RECENT.lock().unwrap().push_back(Recent { make: 0x7A, e0: false, down: true, block: true, at: now });
        let lp_down = (0x7Ai64 << 16) as LPARAM;
        let lp_up = lp_down | (1 << 31);
        assert_eq!(decide(lp_up), 0);
        assert_eq!(decide(lp_down), 1);
        assert_eq!(decide(lp_down), 0);
    }
}
