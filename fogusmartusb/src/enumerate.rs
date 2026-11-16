//! Lista os aparelhos USB juntando quatro fontes: nusb (todos os aparelhos,
//! interfaces e classes), hidapi (coleções HID com usage page), serialport
//! (portas COM / tty de cada aparelho) e PC/SC (nomes dos leitores de smart
//! card). Cada aparelho é classificado por `classify`.

use std::collections::{HashMap, HashSet};

use nusb::MaybeFuture;
use serde_json::{Value, json};

use crate::classify::{self, Class, HidCol, Iface, Raw};
use crate::ids::{self, Hint};

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub raw: Raw,
    pub class: Class,
    pub serial: Option<String>,
    pub bus: String,
    pub port_chain: String,
    pub speed: Option<&'static str>,
    pub usb_version: u16,
    pub device_version: u16,
    pub driver: Option<String>,
}

fn bcd(v: u16) -> String {
    format!("{:x}.{:02x}", v >> 8, v & 0xFF)
}

impl Device {
    pub fn vid_pid(&self) -> String {
        format!("{:04x}:{:04x}", self.raw.vid, self.raw.pid)
    }

    pub fn to_json(&self) -> Value {
        let c = &self.class;
        let r = &self.raw;
        json!({
            "id": self.id,
            "vid": format!("{:04x}", r.vid),
            "pid": format!("{:04x}", r.pid),
            "name": c.name,
            "kind": c.kind,
            "kinds": c.kinds,
            "category": c.category,
            "confidence": c.confidence,
            "source": c.source,
            "vendor": c.vendor,
            "product": c.product,
            "serial": self.serial,
            "modes": c.modes,
            "ports": r.ports,
            "readers": r.readers,
            "interfaces": r.interfaces.iter().map(|i| json!({
                "number": i.number,
                "class": i.class,
                "class_name": classify::class_name(i.class),
                "subclass": i.subclass,
                "protocol": i.protocol,
                "name": i.name,
            })).collect::<Vec<_>>(),
            "hid": r.hid.iter().map(|h| json!({
                "usage_page": h.usage_page,
                "usage": h.usage,
                "usage_name": classify::hid_usage_name(h.usage_page, h.usage),
                "interface": h.interface,
                "path": h.path,
            })).collect::<Vec<_>>(),
            "class": r.class,
            "class_name": classify::class_name(r.class),
            "bus": self.bus,
            "port_chain": self.port_chain,
            "speed": self.speed,
            "usb_version": bcd(self.usb_version),
            "device_version": bcd(self.device_version),
            "driver": self.driver,
        })
    }

    /// Aceita como seletor: o id, `"vvvv:pppp"`, `"vvvv:pppp:serial"` ou uma
    /// porta (`COM5`, `/dev/ttyACM0`).
    pub fn matches(&self, sel: &str) -> bool {
        let s = sel.trim().to_ascii_lowercase();
        if s == self.id.to_ascii_lowercase() || s == self.vid_pid() {
            return true;
        }
        if let Some(serial) = &self.serial
            && s == format!("{}:{}", self.vid_pid(), serial.to_ascii_lowercase())
        {
            return true;
        }
        self.raw.ports.iter().any(|p| p.to_ascii_lowercase() == s)
    }
}

fn speed_name(s: Option<nusb::Speed>) -> Option<&'static str> {
    Some(match s? {
        nusb::Speed::Low => "low",
        nusb::Speed::Full => "full",
        nusb::Speed::High => "high",
        nusb::Speed::Super => "super",
        nusb::Speed::SuperPlus => "super_plus",
        _ => "unknown",
    })
}

/// Barramento curto: no Windows o bus_id é o caminho do controlador
/// (`PCIROOT(0)#PCI(1400)#USBROOT(0)`); fica só a parte PCI (`1400`).
pub fn short_bus(bus: &str) -> String {
    let pci: Vec<&str> = bus.split('#').filter_map(|p| p.strip_prefix("PCI(").and_then(|p| p.strip_suffix(')'))).collect();
    if pci.is_empty() { bus.to_string() } else { pci.join("_").to_ascii_lowercase() }
}

fn device_id(d: &nusb::DeviceInfo) -> String {
    let bus = short_bus(d.bus_id());
    let chain = d.port_chain();
    if chain.is_empty() {
        format!("{bus}-a{}", d.device_address())
    } else {
        let chain: Vec<String> = chain.iter().map(|p| p.to_string()).collect();
        format!("{bus}-{}", chain.join("."))
    }
}

/// Só os ids dos aparelhos presentes (barato: usado para detectar mudanças).
pub fn quick_ids(include_hubs: bool) -> Result<HashSet<String>, String> {
    let list = nusb::list_devices().wait().map_err(|e| format!("cannot list USB devices: {e}"))?;
    Ok(list.filter(|d| include_hubs || d.class() != 0x09).map(|d| device_id(&d)).collect())
}

struct HidEntry {
    vid: u16,
    pid: u16,
    serial: Option<String>,
    col: HidCol,
}

fn hid_entries() -> Vec<HidEntry> {
    let Ok(api) = hidapi::HidApi::new() else { return Vec::new() };
    api.device_list()
        .filter(|d| matches!(d.bus_type(), hidapi::BusType::Usb | hidapi::BusType::Unknown))
        .map(|d| HidEntry {
            vid: d.vendor_id(),
            pid: d.product_id(),
            serial: d.serial_number().map(str::to_string).filter(|s| !s.is_empty()),
            col: HidCol {
                usage_page: d.usage_page(),
                usage: d.usage(),
                interface: d.interface_number(),
                path: d.path().to_string_lossy().into_owned(),
            },
        })
        .collect()
}

struct PortEntry {
    vid: u16,
    pid: u16,
    serial: Option<String>,
    name: String,
}

fn serial_ports() -> Vec<PortEntry> {
    let Ok(ports) = serialport::available_ports() else { return Vec::new() };
    ports
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => {
                Some(PortEntry { vid: u.vid, pid: u.pid, serial: u.serial_number.filter(|s| !s.is_empty()), name: p.port_name })
            }
            _ => None,
        })
        .collect()
}

pub fn pcsc_readers() -> Vec<String> {
    let Ok(ctx) = pcsc::Context::establish(pcsc::Scope::User) else { return Vec::new() };
    match ctx.list_readers_len() {
        Ok(len) => {
            let mut buf = vec![0u8; len];
            match ctx.list_readers(&mut buf) {
                Ok(names) => names.map(|n| n.to_string_lossy().into_owned()).collect(),
                Err(_) => Vec::new(),
            }
        }
        Err(_) => Vec::new(),
    }
}

/// Mesmo aparelho: VID/PID iguais e, se os dois lados têm número de série,
/// séries iguais.
fn same(vid: u16, pid: u16, serial: &Option<String>, dev: &Raw, dev_serial: &Option<String>) -> bool {
    if vid != dev.vid || pid != dev.pid {
        return false;
    }
    match (serial, dev_serial) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

const GENERIC_WORDS: &[&str] = &[
    "usb", "smart", "card", "smartcard", "reader", "interface", "inc", "ltd", "co", "corp", "ccid", "picc", "sam", "contactless", "the", "and",
    "device", "technology", "technologies", "systems", "microsoft", "usbccid", "wudf",
];

fn tokens(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1 && !GENERIC_WORDS.contains(t) && !t.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

/// Associa os leitores PC/SC aos aparelhos com interface de smart card.
fn assign_readers(devs: &mut [Device], readers: Vec<String>) {
    let candidates: Vec<usize> = devs
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            d.raw.interfaces.iter().any(|i| i.class == 0x0B)
                || d.raw.class == 0x0B
                || matches!(ids::hint(d.raw.vid), Hint::SmartCard | Hint::Token)
        })
        .map(|(i, _)| i)
        .collect();
    let mut left = Vec::new();
    for r in readers {
        let rt = tokens(&r);
        let best = candidates
            .iter()
            .map(|&i| {
                let d = &devs[i];
                let mut words = String::new();
                for s in [&d.raw.product, &d.raw.manufacturer, &d.class.vendor, &d.class.product].into_iter().flatten() {
                    words.push_str(s);
                    words.push(' ');
                }
                (i, tokens(&words).intersection(&rt).count())
            })
            .filter(|&(_, n)| n > 0)
            .max_by_key(|&(_, n)| n);
        match best {
            Some((i, _)) => devs[i].raw.readers.push(r),
            None => left.push(r),
        }
    }
    // Um único leitor físico sem nome casado fica com os que sobraram.
    if candidates.len() == 1 {
        devs[candidates[0]].raw.readers.extend(left);
    }
}

pub fn list(include_hubs: bool) -> Result<Vec<Device>, String> {
    let infos: Vec<nusb::DeviceInfo> = nusb::list_devices().wait().map_err(|e| format!("cannot list USB devices: {e}"))?.collect();
    let hids = hid_entries();
    let ports = serial_ports();

    let mut devs: Vec<Device> = infos
        .iter()
        .filter(|d| include_hubs || d.class() != 0x09)
        .map(|d| {
            let serial = d.serial_number().map(str::to_string).filter(|s| !s.is_empty());
            let mut raw = Raw {
                vid: d.vendor_id(),
                pid: d.product_id(),
                class: d.class(),
                subclass: d.subclass(),
                protocol: d.protocol(),
                manufacturer: d.manufacturer_string().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                product: d.product_string().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                interfaces: d
                    .interfaces()
                    .map(|i| Iface {
                        number: i.interface_number(),
                        class: i.class(),
                        subclass: i.subclass(),
                        protocol: i.protocol(),
                        name: i.interface_string().map(str::to_string),
                    })
                    .collect(),
                ..Default::default()
            };
            raw.hid = hids.iter().filter(|h| same(h.vid, h.pid, &h.serial, &raw, &serial)).map(|h| h.col.clone()).collect();
            raw.ports = ports.iter().filter(|p| same(p.vid, p.pid, &p.serial, &raw, &serial)).map(|p| p.name.clone()).collect();
            raw.ports.sort();
            raw.ports.dedup();

            #[cfg(windows)]
            let driver = d.driver().map(str::to_string);
            #[cfg(not(windows))]
            let driver = None;

            Device {
                id: device_id(d),
                class: classify::classify(&raw),
                raw,
                serial,
                bus: d.bus_id().to_string(),
                port_chain: d.port_chain().iter().map(|p| p.to_string()).collect::<Vec<_>>().join("."),
                speed: speed_name(d.speed()),
                usb_version: d.usb_version(),
                device_version: d.device_version(),
                driver,
            }
        })
        .collect();

    // PC/SC só se houver um candidato (evita acordar o serviço à toa).
    if devs.iter().any(|d| d.raw.interfaces.iter().any(|i| i.class == 0x0B) || d.raw.class == 0x0B) {
        assign_readers(&mut devs, pcsc_readers());
    }
    for d in devs.iter_mut().filter(|d| !d.raw.readers.is_empty()) {
        d.class = classify::classify(&d.raw);
    }
    devs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(devs)
}

/// Aparelho pelo seletor (ver `Device::matches`).
pub fn find(sel: &str) -> Result<Device, String> {
    list(true)?.into_iter().find(|d| d.matches(sel)).ok_or_else(|| format!("no USB device matches \"{sel}\""))
}

/// Diferença entre duas listas: `(adicionados, removidos)`.
pub fn diff<'a>(old: &'a HashMap<String, Value>, new: &'a HashMap<String, Value>) -> (Vec<&'a Value>, Vec<&'a Value>) {
    let mut added: Vec<&Value> = new.iter().filter(|(k, _)| !old.contains_key(*k)).map(|(_, v)| v).collect();
    let mut removed: Vec<&Value> = old.iter().filter(|(k, _)| !new.contains_key(*k)).map(|(_, v)| v).collect();
    added.sort_by_key(|v| v["id"].as_str().unwrap_or("").to_string());
    removed.sort_by_key(|v| v["id"].as_str().unwrap_or("").to_string());
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_names() {
        assert_eq!(short_bus("PCIROOT(0)#PCI(1400)#USBROOT(0)"), "1400");
        assert_eq!(short_bus("PCIROOT(0)#PCI(1C00)#PCI(0000)#USBROOT(0)"), "1c00_0000");
        assert_eq!(short_bus("3"), "3");
    }

    #[test]
    fn reader_tokens() {
        let t = tokens("Alcor Micro USB Smart Card Reader 0");
        assert_eq!(t, ["alcor", "micro"].iter().map(|s| s.to_string()).collect());
        assert!(tokens("ACS ACR122 0").contains("acr122"));
    }

    #[test]
    fn readers_are_assigned() {
        let mk = |vid, product: &str| {
            let raw = Raw {
                vid,
                pid: 1,
                product: Some(product.into()),
                interfaces: vec![Iface { class: 0x0B, ..Default::default() }],
                ..Default::default()
            };
            Device {
                id: product.into(),
                class: classify::classify(&raw),
                raw,
                serial: None,
                bus: "1".into(),
                port_chain: String::new(),
                speed: None,
                usb_version: 0x200,
                device_version: 0x100,
                driver: None,
            }
        };
        let mut devs = vec![mk(0x058F, "USB Smart Card Reader"), mk(0x072F, "ACR122U PICC Interface")];
        assign_readers(&mut devs, vec!["Alcor Micro USB Smart Card Reader 0".into(), "ACS ACR122U PICC Interface 0".into()]);
        // "Alcor Micro" vem do fabricante na base embutida.
        assert_eq!(devs[0].raw.readers, vec!["Alcor Micro USB Smart Card Reader 0"]);
        assert_eq!(devs[1].raw.readers, vec!["ACS ACR122U PICC Interface 0"]);
    }

    /// Roda no aparelho real: só confere que a enumeração não falha.
    #[test]
    fn list_real_devices() {
        let devs = list(false).expect("list");
        for d in &devs {
            assert!(!d.id.is_empty());
            assert!(!d.class.kind.is_empty());
        }
    }
}
