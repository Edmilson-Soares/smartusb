//! Leitura do descritor de relatório HID, para decodificar scanners HID POS
//! de qualquer fabricante pelo que o próprio aparelho declara: onde ficam
//! os dados lidos (usage 0x8C:0xFE "Decoded Data"), o identificador de
//! simbologia (0x8C:0xFB..0xFD) e o bit de continuação (0x8C:0xFF).

/// Um campo de relatório de entrada.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub report_id: u8,
    /// Posição em bits a partir do início do relatório, sem o byte do número.
    pub bit_offset: usize,
    pub size: usize,
    pub count: usize,
    /// Usages completos (`page << 16 | usage`), um por elemento (o último se
    /// repete quando há menos usages que elementos).
    pub usages: Vec<u32>,
    pub constant: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Descriptor {
    pub inputs: Vec<Field>,
    pub uses_report_ids: bool,
}

#[derive(Clone, Default)]
struct Globals {
    page: u32,
    size: usize,
    count: usize,
    report_id: u8,
}

fn value(data: &[u8]) -> u32 {
    data.iter().rev().fold(0u32, |acc, &b| (acc << 8) | b as u32)
}

pub fn parse(desc: &[u8]) -> Result<Descriptor, String> {
    let mut out = Descriptor::default();
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let mut usage_min: Option<u32> = None;
    let mut offsets: std::collections::HashMap<u8, usize> = Default::default();
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Item longo: tamanho no byte seguinte.
            let len = *desc.get(i + 1).ok_or("truncated long item")? as usize;
            i += 3 + len;
            continue;
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        let data = desc.get(i + 1..i + 1 + size).ok_or("truncated HID report descriptor")?;
        let v = value(data);
        let kind = (prefix >> 2) & 3;
        let tag = prefix >> 4;
        i += 1 + size;
        let full = |u: u32, page: u32| if size == 4 { u } else { (page << 16) | u };
        match (kind, tag) {
            // Main
            (0, 8) => {
                let constant = v & 1 != 0;
                let off = offsets.entry(g.report_id).or_insert(0);
                let mut list = usages.clone();
                if list.is_empty() && !constant {
                    list.push(g.page << 16);
                }
                out.inputs.push(Field { report_id: g.report_id, bit_offset: *off, size: g.size, count: g.count, usages: list, constant });
                *off += g.size * g.count;
                usages.clear();
                usage_min = None;
            }
            (0, _) => {
                // Output, Feature, Collection, End Collection: limpa os locais.
                usages.clear();
                usage_min = None;
            }
            // Global
            (1, 0) => g.page = v,
            (1, 7) => g.size = v as usize,
            (1, 8) => {
                g.report_id = v as u8;
                out.uses_report_ids = true;
            }
            (1, 9) => g.count = v as usize,
            (1, 10) => stack.push(g.clone()),
            (1, 11) => g = stack.pop().unwrap_or_default(),
            (1, _) => {}
            // Local
            (2, 0) => usages.push(full(v, g.page)),
            (2, 1) => usage_min = Some(full(v, g.page)),
            (2, 2) => {
                if let Some(min) = usage_min.take() {
                    let max = full(v, g.page);
                    // Faixas enormes (teclas, por exemplo) não interessam aqui.
                    if max >= min && max - min < 1024 {
                        usages.extend(min..=max);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

impl Field {
    pub fn usage_at(&self, i: usize) -> Option<u32> {
        self.usages.get(i).or(self.usages.last()).copied()
    }
}

const DECODED_DATA: u32 = 0x008C_00FE;
const CONTINUED: u32 = 0x008C_00FF;
const SYMBOLOGY: [u32; 3] = [0x008C_00FB, 0x008C_00FC, 0x008C_00FD];

/// Onde estão os dados de leitura num relatório HID POS.
#[derive(Clone, Debug, PartialEq)]
pub struct BarcodeLayout {
    pub report_id: u8,
    pub uses_report_ids: bool,
    /// (bit, tamanho em bits) de cada byte de dados.
    pub data: Vec<(usize, usize)>,
    pub symbology: Vec<(usize, usize)>,
    pub continued: Option<(usize, usize)>,
}

pub fn barcode_layout(d: &Descriptor) -> Option<BarcodeLayout> {
    // O relatório que tem "Decoded Data".
    let rid = d.inputs.iter().find(|f| f.usages.contains(&DECODED_DATA))?.report_id;
    let mut l = BarcodeLayout { report_id: rid, uses_report_ids: d.uses_report_ids, data: Vec::new(), symbology: Vec::new(), continued: None };
    let mut sym: Vec<(u32, usize, usize)> = Vec::new();
    for f in d.inputs.iter().filter(|f| f.report_id == rid && !f.constant) {
        for i in 0..f.count {
            let bit = f.bit_offset + i * f.size;
            match f.usage_at(i) {
                Some(DECODED_DATA) => l.data.push((bit, f.size)),
                Some(CONTINUED) => l.continued = Some((bit, f.size)),
                Some(u) if SYMBOLOGY.contains(&u) => sym.push((u, bit, f.size)),
                _ => {}
            }
        }
    }
    sym.sort();
    l.symbology = sym.into_iter().map(|(_, b, s)| (b, s)).collect();
    (!l.data.is_empty()).then_some(l)
}

fn bits(payload: &[u8], bit: usize, size: usize) -> u32 {
    let mut v = 0u32;
    for k in 0..size.min(32) {
        let b = bit + k;
        if payload.get(b / 8).is_some_and(|byte| byte >> (b % 8) & 1 != 0) {
            v |= 1 << k;
        }
    }
    v
}

/// Uma parte de leitura decodificada de um relatório.
#[derive(Debug, PartialEq)]
pub struct Chunk {
    pub data: Vec<u8>,
    pub symbology: Vec<u8>,
    pub continued: bool,
}

impl BarcodeLayout {
    /// `report` como o hidapi entrega (com o número do relatório no 1º byte
    /// quando o aparelho usa números). None se for outro relatório.
    pub fn decode(&self, report: &[u8]) -> Option<Chunk> {
        let payload = if self.uses_report_ids {
            if report.first() != Some(&self.report_id) {
                return None;
            }
            &report[1..]
        } else {
            report
        };
        let byte = |(b, s): (usize, usize)| bits(payload, b, s) as u8;
        let mut data: Vec<u8> = self.data.iter().map(|&x| byte(x)).collect();
        while data.last() == Some(&0) {
            data.pop();
        }
        Some(Chunk {
            data,
            symbology: self.symbology.iter().map(|&x| byte(x)).filter(|&b| b != 0).collect(),
            continued: self.continued.is_some_and(|c| bits(payload, c.0, c.1) != 0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Descritor no estilo da especificação HID POS: número 2, três bytes de
    /// simbologia, 56 bytes de dados e um bit de continuação.
    fn pos_descriptor() -> Vec<u8> {
        vec![
            0x05, 0x8C, // Usage Page (Bar Code Scanner)
            0x09, 0x02, // Usage (Bar Code Scanner)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x02, //   Report ID 2
            0x09, 0x12, //   Usage (Scanned Data Report)
            0xA1, 0x02, //   Collection (Logical)
            0x15, 0x00, 0x26, 0xFF, 0x00, // Logical 0..255
            0x75, 0x08, 0x95, 0x01, // 8 bits x 1
            0x09, 0xFB, 0x81, 0x02, // SI 1
            0x09, 0xFC, 0x81, 0x02, // SI 2
            0x09, 0xFD, 0x81, 0x02, // SI 3
            0x95, 0x38, 0x09, 0xFE, 0x82, 0x02, 0x01, // 56 x Decoded Data (buffered bytes)
            0x75, 0x01, 0x95, 0x01, 0x09, 0xFF, 0x81, 0x02, // Continued
            0x75, 0x07, 0x81, 0x01, // padding
            0xC0, 0xC0,
            // Um relatório de status (outro número) que deve ser ignorado.
            0x85, 0x03, 0x75, 0x08, 0x95, 0x01, 0x09, 0x15, 0x81, 0x02,
        ]
    }

    #[test]
    fn parses_pos_layout() {
        let d = parse(&pos_descriptor()).unwrap();
        assert!(d.uses_report_ids);
        let l = barcode_layout(&d).unwrap();
        assert_eq!(l.report_id, 2);
        assert_eq!(l.data.len(), 56);
        assert_eq!(l.data[0], (24, 8));
        assert_eq!(l.symbology, vec![(0, 8), (8, 8), (16, 8)]);
        assert_eq!(l.continued, Some((24 + 56 * 8, 1)));

        let mut r = vec![0u8; 1 + 3 + 56 + 1];
        r[0] = 2;
        r[1..4].copy_from_slice(b"]Q1");
        r[4..9].copy_from_slice(b"HELLO");
        r[60] = 1;
        let c = l.decode(&r).unwrap();
        assert_eq!(c.data, b"HELLO");
        assert_eq!(c.symbology, b"]Q1");
        assert!(c.continued);
        assert!(l.decode(&[3, 0]).is_none());
    }

    #[test]
    fn no_barcode_fields() {
        // Teclado de boot: nada de HID POS.
        let kb = [0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xC0];
        let d = parse(&kb).unwrap();
        assert_eq!(d.inputs.len(), 1);
        assert_eq!(d.inputs[0].usages.len(), 8);
        assert!(barcode_layout(&d).is_none());
        assert!(parse(&[0x05]).is_err());
    }
}
