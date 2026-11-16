//! Acesso HID genérico: relatórios de entrada/saída e de feature, para
//! aparelhos que não são scanners (balanças, leitores de tarja, botões,
//! displays de cliente, aparelhos do fabricante...).

use std::ffi::CString;

use serde_json::{Value, json};

use crate::enumerate;

pub struct Hid {
    pub dev: hidapi::HidDevice,
    pub path: String,
    pub usage_page: u16,
    pub usage: u16,
}

/// `selector`: caminho HID, ou um seletor de aparelho (id, `vvvv:pppp`).
/// `opts`: `{ "usage_page", "usage", "interface" }` para escolher a coleção.
pub fn open(selector: &str, opts: &Value) -> Result<Hid, String> {
    let api = hidapi::HidApi::new().map_err(|e| format!("hidapi: {e}"))?;
    let num = |k: &str| opts.get(k).and_then(Value::as_i64);
    let sel = selector.trim();

    let found = api.device_list().find(|d| d.path().to_string_lossy() == sel).map(|d| (d.path().to_owned(), d.usage_page(), d.usage()));
    let (path, page, usage) = match found {
        Some(x) => x,
        None => {
            let dev = enumerate::find(sel)?;
            let col = dev
                .raw
                .hid
                .iter()
                .filter(|h| num("usage_page").is_none_or(|p| h.usage_page as i64 == p))
                .filter(|h| num("usage").is_none_or(|u| h.usage as i64 == u))
                .filter(|h| num("interface").is_none_or(|i| h.interface as i64 == i))
                // Sem filtro, evita teclado e mouse (o sistema os segura no Windows).
                .min_by_key(|h| (h.usage_page == 1 && matches!(h.usage, 2 | 6), h.interface))
                .ok_or_else(|| format!("{} ({}) has no matching HID collection", dev.class.name, dev.vid_pid()))?;
            let c = CString::new(col.path.clone()).map_err(|_| "invalid HID path".to_string())?;
            (c, col.usage_page, col.usage)
        }
    };
    let dev = api.open_path(&path).map_err(|e| format!("cannot open HID device: {e}"))?;
    Ok(Hid { dev, path: path.to_string_lossy().into_owned(), usage_page: page, usage })
}

impl Hid {
    pub fn info(&self) -> Value {
        let info = self.dev.get_device_info().ok();
        json!({
            "path": self.path,
            "usage_page": self.usage_page,
            "usage": self.usage,
            "vid": info.as_ref().map(|i| format!("{:04x}", i.vendor_id())),
            "pid": info.as_ref().map(|i| format!("{:04x}", i.product_id())),
            "product": info.as_ref().and_then(|i| i.product_string().map(str::to_string)),
            "manufacturer": info.as_ref().and_then(|i| i.manufacturer_string().map(str::to_string)),
            "serial": info.as_ref().and_then(|i| i.serial_number().map(str::to_string)),
        })
    }
}
