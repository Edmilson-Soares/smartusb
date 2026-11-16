//! Testa o modo teclado exclusivo de ponta a ponta com a DLL de verdade
//! (o hook global precisa de DLL): carrega native/fogusmartusb.dll, abre um
//! scanner exclusivo que aceita teclas injetadas, injeta Shift (inofensivo
//! se vazar) e confere que o hook as engoliu.
//!   ./build.ps1; cargo run --example exclusive_selftest
#[cfg(windows)]
fn main() {
    use std::ffi::{CString, c_char};
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

    type Open = unsafe extern "C" fn(*const c_char, *const c_char) -> i64;
    type Info = unsafe extern "C" fn(i64, *mut u8, i64) -> i64;
    type Close = extern "C" fn(i64) -> i32;
    type LastError = unsafe extern "C" fn(*mut u8, i64) -> i64;

    let path: Vec<u16> = "../native/fogusmartusb.dll".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let lib = LoadLibraryW(path.as_ptr());
        assert!(!lib.is_null(), "build the DLL first (build.ps1)");
        let f = |name: &str| GetProcAddress(lib, CString::new(name).unwrap().as_ptr() as _).expect(name);
        let open: Open = std::mem::transmute(f("fsu_scanner_open"));
        let info: Info = std::mem::transmute(f("fsu_scanner_info"));
        let close: Close = std::mem::transmute(f("fsu_scanner_close"));
        let last_error: LastError = std::mem::transmute(f("fsu_last_error"));

        let sel = CString::new("keyboard").unwrap();
        let opts = CString::new(r#"{"exclusive":true,"synthetic":true}"#).unwrap();
        let h = open(sel.as_ptr(), opts.as_ptr());
        let mut buf = vec![0u8; 65536];
        if h < 0 {
            let n = last_error(buf.as_mut_ptr(), buf.len() as i64);
            panic!("open failed: {}", String::from_utf8_lossy(&buf[..n as usize]));
        }
        let blocked = |buf: &mut Vec<u8>| {
            let n = info(h, buf.as_mut_ptr(), buf.len() as i64);
            let v: serde_json::Value = serde_json::from_slice(&buf[..n as usize]).unwrap();
            v["blocked"].as_u64().unwrap()
        };
        let before = blocked(&mut buf);

        let key = |up: bool| {
            let mut i: INPUT = std::mem::zeroed();
            i.r#type = INPUT_KEYBOARD;
            i.Anonymous.ki.wScan = 0x2A; // Shift esquerdo
            i.Anonymous.ki.dwFlags = KEYEVENTF_SCANCODE | if up { KEYEVENTF_KEYUP } else { 0 };
            i
        };
        for _ in 0..3 {
            let inputs = [key(false), key(true)];
            SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        let after = blocked(&mut buf);
        let n = info(h, buf.as_mut_ptr(), buf.len() as i64);
        let v: serde_json::Value = serde_json::from_slice(&buf[..n as usize]).unwrap();
        println!("diagnóstico: {}", v["hook_stats"]);
        close(h);
        println!("teclas engolidas pelo hook: {} (de 6 eventos injetados)", after - before);
        if after > before {
            println!("OK: modo exclusivo funcionando");
        } else {
            println!("FALHOU: nenhuma tecla foi bloqueada (o programa em foco é 32 bits, elevado, ou não usa fila de mensagens?)");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    println!("só no Windows");
}
