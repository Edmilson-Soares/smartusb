//! Leitores de digital pelo Windows Biometric Framework (WinBio): funciona
//! com qualquer leitor que tenha driver WBF (os integrados de notebook, e os
//! USB da DigitalPersona/HID, SecuGen, Futronic, Goodix, Synaptics... que
//! trazem driver para o Windows Hello).
//!
//! - `units`: leitores disponíveis.
//! - `identify`: espera um dedo e diz de quem é (a conta do Windows com
//!   digital cadastrada no Windows Hello) e qual dedo.
//! - `capture`: imagem crua da digital (ANSI INCITS 381), para guardar ou
//!   usar com um extrator/comparador próprio. O Windows exige privilégio de
//!   administrador para captura crua.
//!
//! O WinBio só entrega o sensor ao programa em primeiro plano; um programa
//! de console pede o foco com `WinBioAcquireFocus`, que também exige
//! administrador. Sem foco, `identify` espera até o tempo acabar.

use serde_json::{Value, json};
use std::time::Duration;

/// Imagem de digital decodificada do registro ANSI INCITS 381.
#[derive(Debug, PartialEq)]
pub struct FingerImage {
    pub width: u16,
    pub height: u16,
    pub depth: u8,
    pub dpi_x: u16,
    pub dpi_y: u16,
    pub finger: u8,
    pub quality: u8,
    pub compression: u8,
    pub pixels: Vec<u8>,
}

fn be(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, &x| (a << 8) | x as u64)
}

/// Registro ANSI INCITS 381-2004 ("FIR\0"): cabeçalho geral de 36 bytes,
/// cabeçalho do dedo de 14 bytes, imagem.
pub fn parse_ansi381(rec: &[u8]) -> Result<FingerImage, String> {
    if rec.len() < 50 || &rec[0..4] != b"FIR\0" {
        return Err("not an ANSI 381 finger image record".into());
    }
    let finger = &rec[36..50];
    let data_len = be(&finger[0..4]) as usize;
    let width = be(&finger[9..11]) as u16;
    let height = be(&finger[11..13]) as u16;
    let depth = rec[32];
    let img_start = 50;
    let img_len = data_len.saturating_sub(14);
    let pixels = rec.get(img_start..img_start + img_len).ok_or("truncated finger image")?.to_vec();
    Ok(FingerImage {
        width,
        height,
        depth,
        dpi_x: be(&rec[28..30]) as u16,
        dpi_y: be(&rec[30..32]) as u16,
        finger: finger[4],
        quality: finger[7],
        compression: rec[33],
        pixels,
    })
}

/// Imagem em tons de cinza (8 bits, sem compressão) para PGM ou BMP.
pub fn save_image(img: &FingerImage, path: &str) -> Result<(), String> {
    if img.depth != 8 || img.compression != 0 {
        return Err(format!("cannot save image: depth {} / compression {} (only 8-bit uncompressed)", img.depth, img.compression));
    }
    let (w, h) = (img.width as usize, img.height as usize);
    if img.pixels.len() < w * h {
        return Err("image data is shorter than width x height".into());
    }
    let out = if path.to_ascii_lowercase().ends_with(".bmp") {
        let row = (w + 3) & !3;
        let palette = 256 * 4;
        let offset = 14 + 40 + palette;
        let size = offset + row * h;
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&(size as u32).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&(offset as u32).to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&(w as i32).to_le_bytes());
        b.extend_from_slice(&(h as i32).to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8u16.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&((row * h) as u32).to_le_bytes());
        let ppm = |dpi: u16| ((dpi as f64) / 0.0254).round() as u32;
        b.extend_from_slice(&ppm(img.dpi_x).to_le_bytes());
        b.extend_from_slice(&ppm(img.dpi_y).to_le_bytes());
        b.extend_from_slice(&256u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        for i in 0..=255u8 {
            b.extend_from_slice(&[i, i, i, 0]);
        }
        for y in (0..h).rev() {
            b.extend_from_slice(&img.pixels[y * w..y * w + w]);
            b.extend(std::iter::repeat_n(0, row - w));
        }
        b
    } else {
        let mut b = format!("P5\n{w} {h}\n255\n").into_bytes();
        b.extend_from_slice(&img.pixels[..w * h]);
        b
    };
    std::fs::write(path, out).map_err(|e| format!("cannot write {path}: {e}"))
}

pub fn finger_name(n: u8) -> &'static str {
    match n {
        1 => "right_thumb",
        2 => "right_index",
        3 => "right_middle",
        4 => "right_ring",
        5 => "right_little",
        6 => "left_thumb",
        7 => "left_index",
        8 => "left_middle",
        9 => "left_ring",
        10 => "left_little",
        _ => "unknown",
    }
}

/// SID binário -> "S-1-5-21-...".
pub fn sid_string(b: &[u8]) -> Option<String> {
    if b.len() < 8 {
        return None;
    }
    let count = b[1] as usize;
    if b.len() < 8 + count * 4 {
        return None;
    }
    let mut s = format!("S-{}-{}", b[0], be(&b[2..8]));
    for i in 0..count {
        let o = 8 + i * 4;
        s.push_str(&format!("-{}", u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])));
    }
    Some(s)
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::sync::mpsc::channel;
    use windows_sys::Win32::Devices::BiometricFramework::*;

    const FINGERPRINT: u32 = 0x0000_0008;
    const FLAG_DEFAULT: u32 = 0;
    const FLAG_RAW: u32 = 1;
    const PURPOSE_IDENTIFY: u8 = 2;
    const ID_TYPE_SID: u32 = 3;
    const ID_TYPE_GUID: u32 = 2;
    const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;

    fn hr_text(hr: i32) -> String {
        let code = hr as u32;
        let what = match hr {
            WINBIO_E_NO_MATCH => "the finger did not match",
            WINBIO_E_UNKNOWN_ID => "fingerprint not enrolled in Windows Hello",
            WINBIO_E_BAD_CAPTURE => "bad capture, try again",
            WINBIO_E_CANCELED => "canceled",
            E_ACCESSDENIED => "access denied (raw capture and focus need administrator rights)",
            _ if code == 0x8009_8037 => "no fingerprint reader found (WINBIO_E_NO_CAPTURE...)",
            _ => "WinBio error",
        };
        format!("{what} (0x{code:08X})")
    }

    fn wstr(w: &[u16]) -> String {
        let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..n])
    }

    pub fn units() -> Result<Vec<Value>, String> {
        unsafe {
            let mut arr: *mut WINBIO_UNIT_SCHEMA = std::ptr::null_mut();
            let mut n: usize = 0;
            let hr = WinBioEnumBiometricUnits(FINGERPRINT, &mut arr, &mut n);
            if hr < 0 {
                return Err(format!("cannot list fingerprint readers: {}", hr_text(hr)));
            }
            let list = if arr.is_null() { &[][..] } else { std::slice::from_raw_parts(arr, n) };
            let out = list
                .iter()
                .map(|u| {
                    json!({
                        "unit_id": u.UnitId,
                        "description": wstr(&u.Description),
                        "manufacturer": wstr(&u.Manufacturer),
                        "model": wstr(&u.Model),
                        "serial": wstr(&u.SerialNumber),
                        "device_instance_id": wstr(&u.DeviceInstanceId),
                        "sensor_subtype": u.SensorSubType,
                        "pool": match u.PoolType { 1 => "system", 2 => "private", _ => "unknown" },
                        "firmware": format!("{}.{}", u.FirmwareVersion.MajorVersion, u.FirmwareVersion.MinorVersion),
                    })
                })
                .collect();
            if !arr.is_null() {
                WinBioFree(arr.cast());
            }
            Ok(out)
        }
    }

    /// Sessão com cancelamento por tempo (o WinBio bloqueia até um dedo).
    fn with_session<T>(flags: u32, timeout: Option<Duration>, f: impl FnOnce(u32) -> Result<T, i32>) -> Result<Option<T>, String> {
        unsafe {
            let mut session: u32 = 0;
            // WINBIO_DB_DEFAULT é literalmente ((GUID*)1) na API.
            #[allow(clippy::manual_dangling_ptr)]
            let db_default = 1usize as *const windows_sys::core::GUID;
            let hr = WinBioOpenSession(FINGERPRINT, WINBIO_POOL_SYSTEM, flags, std::ptr::null(), 0, db_default, &mut session);
            if hr < 0 {
                return Err(format!("cannot open biometric session: {}", hr_text(hr)));
            }
            // Sem foco o sensor não responde a programas de console; tenta.
            let focus = WinBioAcquireFocus() >= 0;
            let (tx, rx) = channel::<()>();
            let timer = timeout.map(|t| {
                std::thread::spawn(move || {
                    if rx.recv_timeout(t).is_err() {
                        WinBioCancel(session);
                    }
                })
            });
            let r = f(session);
            let _ = tx.send(());
            if let Some(t) = timer {
                let _ = t.join();
            }
            if focus {
                WinBioReleaseFocus();
            }
            WinBioCloseSession(session);
            match r {
                Ok(v) => Ok(Some(v)),
                Err(WINBIO_E_CANCELED) => Ok(None),
                Err(hr) => Err(hr_text(hr)),
            }
        }
    }

    pub fn identify(timeout: Option<Duration>) -> Result<Option<Value>, String> {
        let r = with_session(FLAG_DEFAULT, timeout, |s| unsafe {
            let mut unit: u32 = 0;
            let mut id: WINBIO_IDENTITY = std::mem::zeroed();
            let mut sub: u8 = 0;
            let mut reject: u32 = 0;
            let hr = WinBioIdentify(s, &mut unit, &mut id, &mut sub, &mut reject);
            match hr {
                h if h >= 0 => Ok(Some((unit, id, sub))),
                WINBIO_E_UNKNOWN_ID | WINBIO_E_NO_MATCH => Ok(None),
                h => Err(h),
            }
        })?;
        Ok(r.map(|m| match m {
            Some((unit, id, sub)) => {
                let (kind, sid, account) = match id.Type {
                    ID_TYPE_SID => unsafe {
                        let sid = &id.Value.AccountSid;
                        let bytes = &sid.Data[..(sid.Size as usize).min(68)];
                        let account = lookup_account(bytes);
                        ("sid", sid_string(bytes), account)
                    },
                    ID_TYPE_GUID => ("guid", None, None),
                    _ => ("other", None, None),
                };
                json!({ "matched": true, "unit_id": unit, "identity_type": kind, "sid": sid, "account": account, "finger": finger_name(sub) })
            }
            None => json!({ "matched": false }),
        }))
    }

    unsafe fn lookup_account(sid: &[u8]) -> Option<String> {
        use windows_sys::Win32::Security::LookupAccountSidW;
        let mut name = [0u16; 256];
        let mut domain = [0u16; 256];
        let (mut nl, mut dl) = (256u32, 256u32);
        let mut use_ = 0;
        let ok = unsafe { LookupAccountSidW(std::ptr::null(), sid.as_ptr() as _, name.as_mut_ptr(), &mut nl, domain.as_mut_ptr(), &mut dl, &mut use_) };
        (ok != 0).then(|| format!("{}\\{}", wstr(&domain), wstr(&name)))
    }

    pub fn capture(timeout: Option<Duration>) -> Result<Option<(u32, Vec<u8>)>, String> {
        with_session(FLAG_RAW, timeout, |s| unsafe {
            let mut unit: u32 = 0;
            let mut bir: *mut WINBIO_BIR = std::ptr::null_mut();
            let mut size: usize = 0;
            let mut reject: u32 = 0;
            let hr = WinBioCaptureSample(s, PURPOSE_IDENTIFY, WINBIO_DATA_FLAG_RAW as u8, &mut unit, &mut bir, &mut size, &mut reject);
            if hr < 0 {
                return Err(hr);
            }
            let base = bir as *const u8;
            let all = std::slice::from_raw_parts(base, size);
            let blk = (*bir).StandardDataBlock;
            let data = all.get(blk.Offset as usize..(blk.Offset + blk.Size) as usize).unwrap_or(&[]).to_vec();
            WinBioFree(bir.cast());
            Ok((unit, data))
        })
    }
}

#[cfg(not(windows))]
mod win {
    use super::*;
    const MSG: &str = "fingerprint readers are only supported on Windows (WinBio); on Linux use fprintd";
    pub fn units() -> Result<Vec<Value>, String> {
        Err(MSG.into())
    }
    pub fn identify(_t: Option<Duration>) -> Result<Option<Value>, String> {
        Err(MSG.into())
    }
    pub fn capture(_t: Option<Duration>) -> Result<Option<(u32, Vec<u8>)>, String> {
        Err(MSG.into())
    }
}

pub use win::{identify, units};

/// Captura e devolve `{ unit_id, width, height, depth, dpi, finger, quality,
/// record (hex do registro ANSI 381), saved }`; salva em `save` (.pgm ou .bmp)
/// se informado. None se o tempo acabar.
pub fn capture(timeout: Option<Duration>, save: &str) -> Result<Option<Value>, String> {
    let Some((unit, rec)) = win::capture(timeout)? else { return Ok(None) };
    let mut v = json!({ "unit_id": unit, "record": crate::to_hex(&rec) });
    match parse_ansi381(&rec) {
        Ok(img) => {
            v["width"] = json!(img.width);
            v["height"] = json!(img.height);
            v["depth"] = json!(img.depth);
            v["dpi"] = json!([img.dpi_x, img.dpi_y]);
            v["finger"] = json!(finger_name(img.finger));
            v["quality"] = json!(img.quality);
            if !save.is_empty() {
                save_image(&img, save)?;
                v["saved"] = json!(save);
            }
        }
        Err(e) => v["format_error"] = json!(e),
    }
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(w: u16, h: u16) -> Vec<u8> {
        let mut r = b"FIR\0".to_vec();
        r.extend_from_slice(b"010\0");
        let total = 36 + 14 + (w as usize * h as usize);
        r.extend_from_slice(&(total as u64).to_be_bytes()[2..]);
        r.extend_from_slice(&[0; 4]); // CBEFF
        r.extend_from_slice(&[0, 0, 0, 31, 1, 1]); // device, level, fingers, units
        r.extend_from_slice(&500u16.to_be_bytes());
        r.extend_from_slice(&500u16.to_be_bytes());
        r.extend_from_slice(&500u16.to_be_bytes());
        r.extend_from_slice(&500u16.to_be_bytes());
        r.extend_from_slice(&[8, 0, 0, 0]); // depth, compression, reserved
        assert_eq!(r.len(), 36);
        let data_len = 14 + (w as u32 * h as u32);
        r.extend_from_slice(&data_len.to_be_bytes());
        r.extend_from_slice(&[2, 1, 0, 80, 0]);
        r.extend_from_slice(&w.to_be_bytes());
        r.extend_from_slice(&h.to_be_bytes());
        r.push(0);
        r.extend((0..w as usize * h as usize).map(|i| i as u8));
        r
    }

    #[test]
    fn ansi381() {
        let img = parse_ansi381(&record(4, 3)).unwrap();
        assert_eq!((img.width, img.height, img.depth, img.dpi_x), (4, 3, 8, 500));
        assert_eq!(img.finger, 2);
        assert_eq!(img.quality, 80);
        assert_eq!(img.pixels.len(), 12);
        assert!(parse_ansi381(b"nope").is_err());
        let dir = std::env::temp_dir();
        for ext in ["pgm", "bmp"] {
            let p = dir.join(format!("fogusmartusb_test.{ext}"));
            save_image(&img, p.to_str().unwrap()).unwrap();
            let b = std::fs::read(&p).unwrap();
            if ext == "bmp" {
                assert_eq!(&b[..2], b"BM");
                assert_eq!(b.len(), 14 + 40 + 1024 + 4 * 3);
            } else {
                assert!(b.starts_with(b"P5\n4 3\n255\n"));
            }
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn sids() {
        // S-1-5-21-1-2-3-1001
        let mut b = vec![1, 5, 0, 0, 0, 0, 0, 5];
        for x in [21u32, 1, 2, 3, 1001] {
            b.extend_from_slice(&x.to_le_bytes());
        }
        assert_eq!(sid_string(&b).unwrap(), "S-1-5-21-1-2-3-1001");
        assert_eq!(finger_name(7), "left_index");
    }

    #[cfg(windows)]
    #[test]
    fn list_units_does_not_fail() {
        // Sem leitor a lista vem vazia; com o serviço desligado, erro claro.
        match units() {
            Ok(list) => assert!(list.iter().all(|u| u["unit_id"].is_number())),
            Err(e) => assert!(e.contains("fingerprint"), "{e}"),
        }
    }
}
