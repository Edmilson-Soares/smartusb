//! Análise do conteúdo lido: identificador de simbologia AIM (`]Q1`...) e o
//! tipo do dado (URL, PIX, chave de NF-e, boleto, GTIN/EAN, CPF/CNPJ, Wi-Fi,
//! vCard, JSON...), com conferência dos dígitos verificadores.

use serde_json::{Map, Value, json};

/// Simbologia pelo identificador AIM (ISO/IEC 15424): `]` + letra + modificador.
pub fn aim_symbology(id: &str) -> Option<&'static str> {
    let mut c = id.chars();
    if c.next() != Some(']') {
        return None;
    }
    Some(match c.next()? {
        'A' => "code39",
        'C' => "code128",
        'd' => "datamatrix",
        'E' => "ean_upc",
        'e' => "gs1_databar",
        'F' => "codabar",
        'G' => "code93",
        'I' => "itf",
        'L' => "pdf417",
        'M' => "msi",
        'Q' => "qrcode",
        'z' => "aztec",
        'U' => "maxicode",
        'H' => "code11",
        'X' => "other",
        _ => return None,
    })
}

/// Separa o prefixo AIM (`]Q1https://...`), se houver.
pub fn split_aim(text: &str) -> (Option<&str>, &str) {
    let b = text.as_bytes();
    if b.len() >= 3 && b[0] == b']' && b[1].is_ascii_alphabetic() && b[2].is_ascii_alphanumeric() && aim_symbology(&text[..3]).is_some() {
        return (Some(&text[..3]), &text[3..]);
    }
    (None, text)
}

fn digits(s: &str) -> Option<Vec<u32>> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(s.bytes().map(|b| (b - b'0') as u32).collect())
}

/// Dígito verificador GS1 (EAN-8/13, UPC-A, GTIN-14, SSCC...).
pub fn gs1_check_ok(s: &str) -> bool {
    let Some(d) = digits(s) else { return false };
    if d.len() < 2 {
        return false;
    }
    let (body, check) = d.split_at(d.len() - 1);
    let sum: u32 = body.iter().rev().enumerate().map(|(i, &x)| if i % 2 == 0 { x * 3 } else { x }).sum();
    (10 - sum % 10) % 10 == check[0]
}

pub fn cpf_ok(s: &str) -> bool {
    let Some(d) = digits(s) else { return false };
    if d.len() != 11 || d.iter().all(|&x| x == d[0]) {
        return false;
    }
    let dv = |n: usize| {
        let sum: u32 = (0..n).map(|i| d[i] * (n as u32 + 1 - i as u32)).sum();
        let r = (sum * 10) % 11;
        if r == 10 { 0 } else { r }
    };
    dv(9) == d[9] && dv(10) == d[10]
}

pub fn cnpj_ok(s: &str) -> bool {
    let Some(d) = digits(s) else { return false };
    if d.len() != 14 || d.iter().all(|&x| x == d[0]) {
        return false;
    }
    let dv = |n: usize| {
        let weights: Vec<u32> = if n == 12 { vec![5, 4, 3, 2, 9, 8, 7, 6, 5, 4, 3, 2] } else { vec![6, 5, 4, 3, 2, 9, 8, 7, 6, 5, 4, 3, 2] };
        let sum: u32 = (0..n).map(|i| d[i] * weights[i]).sum();
        let r = sum % 11;
        if r < 2 { 0 } else { 11 - r }
    };
    dv(12) == d[12] && dv(13) == d[13]
}

/// Módulo 11 com pesos 2..9 (chave de NF-e/CT-e/MDF-e, boleto bancário).
fn mod11(d: &[u32]) -> u32 {
    let sum: u32 = d.iter().rev().enumerate().map(|(i, &x)| x * (2 + (i as u32 % 8))).sum();
    sum % 11
}

/// Chave de acesso de documento fiscal eletrônico (44 dígitos).
pub fn nfe_key_ok(s: &str) -> bool {
    let Some(d) = digits(s) else { return false };
    if d.len() != 44 {
        return false;
    }
    let r = mod11(&d[..43]);
    let dv = if r < 2 { 0 } else { 11 - r };
    dv == d[43]
}

/// Código de barras de boleto bancário (44 dígitos, DV geral na posição 5).
pub fn boleto_ok(s: &str) -> bool {
    let Some(d) = digits(s) else { return false };
    if d.len() != 44 || d[0] == 8 {
        return false;
    }
    let mut body = d[..4].to_vec();
    body.extend_from_slice(&d[5..]);
    let r = 11 - mod11(&body);
    let dv = if r == 0 || r == 10 || r == 11 { 1 } else { r };
    dv == d[4]
}

fn mod10(d: &[u32]) -> u32 {
    let sum: u32 = d
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &x)| {
            let v = if i % 2 == 0 { x * 2 } else { x };
            v / 10 + v % 10
        })
        .sum();
    (10 - sum % 10) % 10
}

/// Código de barras de arrecadação (contas de consumo, tributos): começa com 8.
pub fn arrecadacao_ok(s: &str) -> bool {
    let Some(d) = digits(s) else { return false };
    if d.len() != 44 || d[0] != 8 {
        return false;
    }
    let mut body = d[..3].to_vec();
    body.extend_from_slice(&d[4..]);
    match d[2] {
        6 | 7 => mod10(&body) == d[3],
        8 | 9 => {
            let r = mod11(&body);
            let dv = if r <= 1 { 0 } else { 11 - r };
            dv == d[3]
        }
        _ => false,
    }
}

/// Campos TLV do padrão EMV QR (PIX "copia e cola").
pub fn emv_tlv(s: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        if i + 4 > b.len() {
            return None;
        }
        let tag = s.get(i..i + 2)?;
        let len: usize = s.get(i + 2..i + 4)?.parse().ok()?;
        let val = s.get(i + 4..i + 4 + len)?;
        out.push((tag.to_string(), val.to_string()));
        i += 4 + len;
    }
    Some(out)
}

fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn pix(s: &str) -> Option<Value> {
    if !s.starts_with("000201") {
        return None;
    }
    let fields = emv_tlv(s)?;
    let get = |t: &str| fields.iter().find(|f| f.0 == t).map(|f| f.1.clone());
    let crc_ok = s.len() >= 4 && {
        let (body, crc) = s.split_at(s.len() - 4);
        body.ends_with("6304") && u16::from_str_radix(crc, 16).ok() == Some(crc16_ccitt(body.as_bytes()))
    };
    let mut key = None;
    let mut url = None;
    let mut is_pix = false;
    for (tag, val) in &fields {
        let n: u32 = tag.parse().unwrap_or(0);
        if (26..=51).contains(&n)
            && let Some(sub) = emv_tlv(val)
        {
            let gui = sub.iter().find(|f| f.0 == "00").map(|f| f.1.to_ascii_uppercase());
            if gui.as_deref() == Some("BR.GOV.BCB.PIX") {
                is_pix = true;
                key = sub.iter().find(|f| f.0 == "01").map(|f| f.1.clone());
                url = sub.iter().find(|f| f.0 == "25").map(|f| f.1.clone());
            }
        }
    }
    let mut v = json!({
        "type": if is_pix { "pix" } else { "emv_qr" },
        "valid": crc_ok,
        "merchant": get("59"),
        "city": get("60"),
        "amount": get("54"),
        "currency": get("53"),
    });
    if is_pix {
        v["key"] = json!(key);
        v["url"] = json!(url);
        let txid = get("62").and_then(|a| emv_tlv(&a)).and_then(|sub| sub.into_iter().find(|f| f.0 == "05").map(|f| f.1));
        v["txid"] = json!(txid);
    }
    Some(v)
}

fn kv_fields(body: &str, sep: char) -> Map<String, Value> {
    let mut m = Map::new();
    for part in body.split(sep) {
        if let Some((k, v)) = part.split_once(':') {
            m.insert(k.trim().to_lowercase(), json!(v));
        }
    }
    m
}

/// Classifica o texto lido: `{ "type", "valid"?, ... }`.
pub fn analyze(text: &str) -> Value {
    let t = text.trim_matches(|c: char| c == '\r' || c == '\n' || c == '\0');
    if t.is_empty() {
        return json!({ "type": "empty" });
    }
    if let Some(v) = pix(t) {
        return v;
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // QR Code de NFC-e: URL da SEFAZ com o parâmetro p=chave|...
        if let Some(p) = t.split(['?', '&']).find_map(|q| q.strip_prefix("p=").or_else(|| q.strip_prefix("chNFe="))) {
            let key: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            if key.len() == 44 {
                return json!({ "type": "nfce", "url": t, "key": key, "valid": nfe_key_ok(&key) });
            }
        }
        return json!({ "type": "url", "url": t });
    }
    if lower.starts_with("wifi:") {
        let f = kv_fields(&t[5..], ';');
        return json!({ "type": "wifi", "ssid": f.get("s"), "password": f.get("p"), "security": f.get("t"), "hidden": f.get("h") });
    }
    if lower.starts_with("begin:vcard") {
        let mut v = json!({ "type": "vcard" });
        for line in t.lines() {
            let (k, val) = match line.split_once(':') {
                Some(x) => x,
                None => continue,
            };
            let key = k.split(';').next().unwrap_or("").to_ascii_uppercase();
            let name = match key.as_str() {
                "FN" => "name",
                "TEL" => "phone",
                "EMAIL" => "email",
                "ORG" => "org",
                "URL" => "url",
                _ => continue,
            };
            if v.get(name).is_none() {
                v[name] = json!(val.trim());
            }
        }
        return v;
    }
    if lower.starts_with("mailto:") {
        return json!({ "type": "email", "email": &t[7..] });
    }
    if lower.starts_with("tel:") {
        return json!({ "type": "phone", "phone": &t[4..] });
    }
    if lower.starts_with("geo:") {
        return json!({ "type": "geo", "coords": &t[4..] });
    }
    if lower.starts_with("smsto:") || lower.starts_with("sms:") {
        return json!({ "type": "sms", "text": t });
    }
    if lower.starts_with("otpauth://") {
        return json!({ "type": "otp", "url": t });
    }
    if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')) {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            return json!({ "type": "json", "value": v });
        }
    }
    // GS1 com separador GS (FNC1): (01)GTIN(17)validade(10)lote...
    if t.contains('\u{1d}') || (t.starts_with("01") && t.len() > 16 && gs1_check_ok(&t[2..16]) && t[16..].starts_with(['1', '2', '3', '7'])) {
        return gs1(t);
    }
    let compact: String = t.chars().filter(|c| !matches!(c, ' ' | '.' | '-' | '/')).collect();
    if let Some(d) = digits(&compact) {
        let n = d.len();
        let gtin = |kind: &str| json!({ "type": kind, "code": compact, "valid": gs1_check_ok(&compact) });
        return match n {
            8 if gs1_check_ok(&compact) => gtin("ean8"),
            12 if gs1_check_ok(&compact) => gtin("upca"),
            13 if gs1_check_ok(&compact) => gtin("ean13"),
            14 if gs1_check_ok(&compact) && !cnpj_ok(&compact) => gtin("gtin14"),
            11 if cpf_ok(&compact) => json!({ "type": "cpf", "code": compact, "valid": true }),
            14 if cnpj_ok(&compact) => json!({ "type": "cnpj", "code": compact, "valid": true }),
            44 if nfe_key_ok(&compact) => nfe_key(&compact),
            44 if boleto_ok(&compact) => json!({ "type": "boleto", "code": compact, "valid": true, "bank": &compact[..3] }),
            44 if arrecadacao_ok(&compact) => json!({ "type": "arrecadacao", "code": compact, "valid": true }),
            47 => json!({ "type": "boleto_linha", "code": compact }),
            48 if d[0] == 8 => json!({ "type": "arrecadacao_linha", "code": compact }),
            _ => json!({ "type": "number", "code": compact }),
        };
    }
    json!({ "type": "text" })
}

fn nfe_key(k: &str) -> Value {
    let model = match &k[20..22] {
        "55" => "nfe",
        "65" => "nfce",
        "57" => "cte",
        "58" => "mdfe",
        "59" => "sat",
        _ => "dfe",
    };
    json!({
        "type": "nfe_key",
        "model": model,
        "code": k,
        "valid": true,
        "uf": &k[0..2],
        "year_month": &k[2..6],
        "cnpj": &k[6..20],
        "series": &k[22..25],
        "number": &k[25..34],
    })
}

/// Application Identifiers GS1 mais comuns: (tamanho fixo, nome).
fn gs1_ai(ai: &str) -> Option<(Option<usize>, &'static str)> {
    Some(match ai {
        "00" => (Some(18), "sscc"),
        "01" => (Some(14), "gtin"),
        "02" => (Some(14), "content_gtin"),
        "10" => (None, "batch"),
        "11" => (Some(6), "production_date"),
        "13" => (Some(6), "pack_date"),
        "15" => (Some(6), "best_before"),
        "17" => (Some(6), "expiry"),
        "21" => (None, "serial"),
        "30" => (None, "count"),
        "37" => (None, "units"),
        "400" => (None, "order"),
        _ => return None,
    })
}

fn gs1(t: &str) -> Value {
    let s = t.trim_start_matches('\u{1d}');
    let mut fields = Map::new();
    let mut rest = s;
    'outer: while !rest.is_empty() {
        for n in [2usize, 3, 4] {
            let Some(ai) = rest.get(..n) else { break };
            if let Some((len, name)) = gs1_ai(ai) {
                let body = &rest[n..];
                let (val, next) = match len {
                    Some(l) if body.len() >= l => (&body[..l], &body[l..]),
                    Some(_) => break 'outer,
                    None => match body.find('\u{1d}') {
                        Some(p) => (&body[..p], &body[p..]),
                        None => (body, ""),
                    },
                };
                fields.insert(name.to_string(), json!(val));
                rest = next.trim_start_matches('\u{1d}');
                continue 'outer;
            }
        }
        fields.insert("rest".to_string(), json!(rest));
        break;
    }
    let valid = fields.get("gtin").and_then(Value::as_str).map(gs1_check_ok);
    json!({ "type": "gs1", "fields": fields, "valid": valid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aim() {
        assert_eq!(split_aim("]Q1hello"), (Some("]Q1"), "hello"));
        assert_eq!(aim_symbology("]E0"), Some("ean_upc"));
        assert_eq!(split_aim("]xyz"), (None, "]xyz"));
        assert_eq!(split_aim("abc"), (None, "abc"));
    }

    #[test]
    fn check_digits() {
        assert!(gs1_check_ok("7891000315507"));
        assert!(!gs1_check_ok("7891000315508"));
        assert!(gs1_check_ok("036000291452")); // UPC-A
        assert!(cpf_ok("52998224725"));
        assert!(!cpf_ok("11111111111"));
        assert!(cnpj_ok("11222333000181"));
        assert!(!cnpj_ok("11222333000182"));
    }

    #[test]
    fn kinds() {
        assert_eq!(analyze("7891000315507")["type"], "ean13");
        assert_eq!(analyze("529.982.247-25")["type"], "cpf");
        assert_eq!(analyze("11.222.333/0001-81")["type"], "cnpj");
        assert_eq!(analyze("https://example.com/x")["type"], "url");
        assert_eq!(analyze("WIFI:S:Casa;T:WPA;P:segredo;;")["password"], "segredo");
        assert_eq!(analyze("hello world")["type"], "text");
        assert_eq!(analyze("{\"a\":1}")["value"]["a"], 1);
        let v = analyze("BEGIN:VCARD\nVERSION:3.0\nFN:Maria\nTEL;TYPE=cell:+5511999\nEND:VCARD");
        assert_eq!(v["name"], "Maria");
        assert_eq!(v["phone"], "+5511999");
    }

    #[test]
    fn nfe() {
        // Chave fictícia com DV calculado.
        let base = "3524011122233300018155001000000123100000001";
        let r = mod11(&digits(base).unwrap());
        let dv = if r < 2 { 0 } else { 11 - r };
        let key = format!("{base}{dv}");
        let v = analyze(&key);
        assert_eq!(v["type"], "nfe_key");
        assert_eq!(v["model"], "nfe");
        assert_eq!(v["uf"], "35");
        let v = analyze(&format!("https://www.sefaz.rs.gov.br/NFCE/NFCE-COM.aspx?p={key}|2|1|1|ABC"));
        assert_eq!(v["type"], "nfce");
        assert_eq!(v["valid"], true);
    }

    #[test]
    fn boleto() {
        // Código de barras do exemplo da FEBRABAN (banco 001).
        assert!(boleto_ok("00193373700000001000500940144816060680935031"));
        assert_eq!(analyze("00193373700000001000500940144816060680935031")["type"], "boleto");
    }

    #[test]
    fn pix_payload() {
        let body = "00020126330014BR.GOV.BCB.PIX0111529982247255204000053039865406123.455802BR5913FULANO DE TAL6009SAO PAULO62070503***6304";
        let payload = format!("{body}{:04X}", crc16_ccitt(body.as_bytes()));
        let v = analyze(&payload);
        assert_eq!(v["type"], "pix");
        assert_eq!(v["valid"], true);
        assert_eq!(v["key"], "52998224725");
        assert_eq!(v["amount"], "123.45");
        assert_eq!(v["merchant"], "FULANO DE TAL");
        assert_eq!(v["txid"], "***");
        let broken = format!("{body}0000");
        assert_eq!(analyze(&broken)["valid"], false);
    }

    #[test]
    fn gs1_fields() {
        let v = analyze("0107891000315507\u{1d}10LOTE1\u{1d}17251231");
        assert_eq!(v["type"], "gs1");
        assert_eq!(v["fields"]["gtin"], "07891000315507");
        assert_eq!(v["fields"]["batch"], "LOTE1");
        assert_eq!(v["fields"]["expiry"], "251231");
        assert_eq!(v["valid"], true);
        let v = analyze("010789100031550717251231");
        assert_eq!(v["fields"]["expiry"], "251231");
    }
}
