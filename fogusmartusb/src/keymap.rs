//! Decodificação de teclas em texto para scanners em modo teclado (HID
//! keyboard wedge). O scanner manda códigos de tecla, não caracteres; o
//! resultado depende do layout que o scanner emula (quase sempre US). Aqui
//! a tradução é feita pela lib, sem passar pelo layout do sistema, então
//! um scanner em US funciona igual num Windows configurado em ABNT2, e o
//! Caps Lock do sistema não inverte as letras.
//!
//! Suporta Shift, AltGr, teclas mortas (ABNT2), Ctrl+tecla (caracteres de
//! controle, como GS = Ctrl+] dos códigos GS1) e Alt+teclado numérico
//! (`Alt+0065` = Windows-1252, `Alt+65` = CP437), que muitos scanners usam
//! para ficar independentes do layout.

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    Us,
    Abnt2,
}

impl Layout {
    pub fn parse(s: &str) -> Result<Layout, String> {
        match s.to_ascii_lowercase().as_str() {
            "" | "us" | "en-us" => Ok(Layout::Us),
            "abnt2" | "abnt" | "br" | "pt-br" => Ok(Layout::Abnt2),
            other => Err(format!("unknown keyboard layout \"{other}\" (use \"us\" or \"abnt2\")")),
        }
    }
}

// Modificadores no formato do relatório de boot (bit = usage - 0xE0).
const LCTRL: u8 = 1 << 0;
const LSHIFT: u8 = 1 << 1;
const LALT: u8 = 1 << 2;
const RCTRL: u8 = 1 << 4;
const RSHIFT: u8 = 1 << 5;
const RALT: u8 = 1 << 6;

/// Resultado de uma tecla: caractere, tecla morta, ou ação.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Out {
    Char(char),
    Dead(char),
    Enter,
    Tab,
    Backspace,
    None,
}

fn letter(usage: u8) -> Option<char> {
    (0x04..=0x1D).contains(&usage).then(|| (b'a' + usage - 0x04) as char)
}

fn keypad(usage: u8) -> Option<char> {
    Some(match usage {
        0x54 => '/',
        0x55 => '*',
        0x56 => '-',
        0x57 => '+',
        0x59..=0x61 => (b'1' + usage - 0x59) as char,
        0x62 => '0',
        _ => return None,
    })
}

fn keypad_digit(usage: u8) -> Option<u32> {
    match usage {
        0x59..=0x61 => Some((usage - 0x59 + 1) as u32),
        0x62 => Some(0),
        _ => None,
    }
}

fn map_us(usage: u8, shift: bool) -> Out {
    let pair = |a: char, b: char| Out::Char(if shift { b } else { a });
    match usage {
        0x1E..=0x27 => {
            let i = (usage - 0x1E) as usize;
            pair("1234567890".as_bytes()[i] as char, "!@#$%^&*()".as_bytes()[i] as char)
        }
        0x2D => pair('-', '_'),
        0x2E => pair('=', '+'),
        0x2F => pair('[', '{'),
        0x30 => pair(']', '}'),
        0x31 | 0x64 => pair('\\', '|'),
        0x32 => pair('#', '~'),
        0x33 => pair(';', ':'),
        0x34 => pair('\'', '"'),
        0x35 => pair('`', '~'),
        0x36 => pair(',', '<'),
        0x37 => pair('.', '>'),
        0x38 | 0x87 => pair('/', '?'),
        0x63 => Out::Char('.'),
        0x85 => Out::Char(','),
        _ => Out::None,
    }
}

fn map_abnt2(usage: u8, shift: bool, altgr: bool) -> Out {
    if altgr {
        return match usage {
            0x14 => Out::Char('/'),
            0x1A => Out::Char('?'),
            0x08 => Out::Char('°'),
            0x1E => Out::Char('¹'),
            0x1F => Out::Char('²'),
            0x20 => Out::Char('³'),
            0x21 => Out::Char('£'),
            0x22 => Out::Char('¢'),
            0x23 => Out::Char('¬'),
            0x2E => Out::Char('§'),
            0x30 => Out::Char('ª'),
            0x31 | 0x32 => Out::Char('º'),
            _ => Out::None,
        };
    }
    let pair = |a: char, b: char| Out::Char(if shift { b } else { a });
    match usage {
        0x23 if shift => Out::Dead('¨'),
        0x1E..=0x27 => map_us(usage, shift),
        0x2D => pair('-', '_'),
        0x2E => pair('=', '+'),
        0x2F => Out::Dead(if shift { '`' } else { '´' }),
        0x30 => pair('[', '{'),
        0x31 | 0x32 => pair(']', '}'),
        0x33 => pair('ç', 'Ç'),
        0x34 => Out::Dead(if shift { '^' } else { '~' }),
        0x35 => pair('\'', '"'),
        0x36 => pair(',', '<'),
        0x37 => pair('.', '>'),
        0x38 => pair(';', ':'),
        0x87 => pair('/', '?'),
        0x64 => pair('\\', '|'),
        0x63 => Out::Char(','),
        0x85 => Out::Char('.'),
        _ => Out::None,
    }
}

fn map_key(layout: Layout, usage: u8, shift: bool, altgr: bool) -> Out {
    match usage {
        0x28 | 0x58 => return Out::Enter,
        0x2B => return Out::Tab,
        0x2A => return Out::Backspace,
        0x2C => return Out::Char(' '),
        _ => {}
    }
    if let Some(c) = letter(usage) {
        if altgr {
            return if layout == Layout::Abnt2 { map_abnt2(usage, shift, true) } else { Out::None };
        }
        return Out::Char(if shift { c.to_ascii_uppercase() } else { c });
    }
    if let Some(c) = keypad(usage) {
        return Out::Char(c);
    }
    match layout {
        Layout::Us if altgr => Out::None,
        Layout::Us => map_us(usage, shift),
        Layout::Abnt2 => map_abnt2(usage, shift, altgr),
    }
}

/// Acento morto + letra (ABNT2).
fn compose(dead: char, c: char) -> Option<char> {
    let table: &[(char, &str, &str)] = &[
        ('~', "aonAON", "ãõñÃÕÑ"),
        ('^', "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        ('´', "aeiouycAEIOUYC", "áéíóúýćÁÉÍÓÚÝĆ"),
        ('`', "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        ('¨', "aeiouyAEIOU", "äëïöüÿÄËÏÖÜ"),
    ];
    let (_, from, to) = table.iter().find(|t| t.0 == dead)?;
    let i = from.chars().position(|x| x == c)?;
    to.chars().nth(i)
}

const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
const CP1252_80: &str = "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ\u{8d}Ž\u{8f}\u{90}‘’“”•–—˜™š›œ\u{9d}žŸ";

/// Caractere de um código Alt+numérico. Com zero à esquerda é Windows-1252;
/// sem, CP437 (o comportamento do Windows).
pub fn alt_code(digits: &str) -> Option<char> {
    let n: u32 = digits.parse().ok()?;
    let b = (n % 256) as u8;
    if b == 0 {
        return None;
    }
    if digits.starts_with('0') {
        match b {
            0x80..=0x9F => CP1252_80.chars().nth((b - 0x80) as usize),
            _ => Some(b as char),
        }
    } else if b < 0x80 {
        Some(b as char)
    } else {
        CP437_HIGH.chars().nth((b - 0x80) as usize)
    }
}

/// Uma leitura terminada.
#[derive(Clone, Debug, PartialEq)]
pub struct Burst {
    pub text: String,
    pub keys: usize,
    /// Intervalo médio entre teclas, em ms (0 com uma tecla só).
    pub avg_gap_ms: f64,
    pub duration_ms: f64,
    /// `"enter"`, `"tab"` ou `"idle"`.
    pub terminator: &'static str,
}

/// Transforma eventos de tecla (usage HID, pressionada/solta) em leituras.
pub struct Decoder {
    layout: Layout,
    tab_ends: bool,
    mods: u8,
    dead: Option<char>,
    alt_digits: String,
    buf: String,
    first: Option<Instant>,
    last: Option<Instant>,
    keys: usize,
}

impl Decoder {
    pub fn new(layout: Layout, tab_ends: bool) -> Decoder {
        Decoder { layout, tab_ends, mods: 0, dead: None, alt_digits: String::new(), buf: String::new(), first: None, last: None, keys: 0 }
    }

    pub fn pending(&self) -> bool {
        !self.buf.is_empty() || self.dead.is_some()
    }

    fn push(&mut self, c: char, at: Instant) {
        if self.buf.is_empty() {
            self.first = Some(at);
            self.keys = 0;
        }
        self.buf.push(c);
        self.keys += 1;
        self.last = Some(at);
    }

    fn finish(&mut self, terminator: &'static str) -> Option<Burst> {
        if let Some(d) = self.dead.take() {
            let at = self.last.unwrap_or_else(Instant::now);
            self.push(d, at);
        }
        if self.buf.is_empty() {
            return None;
        }
        let duration = match (self.first, self.last) {
            (Some(a), Some(b)) => b.duration_since(a).as_secs_f64() * 1000.0,
            _ => 0.0,
        };
        let keys = self.keys;
        let avg = if keys > 1 { duration / (keys - 1) as f64 } else { 0.0 };
        Some(Burst { text: std::mem::take(&mut self.buf), keys, avg_gap_ms: avg, duration_ms: duration, terminator })
    }

    /// Termina a leitura se não chega tecla há `idle_ms`.
    pub fn poll(&mut self, now: Instant, idle_ms: u64) -> Option<Burst> {
        let last = self.last?;
        if self.pending() && now.duration_since(last).as_millis() as u64 >= idle_ms {
            return self.finish("idle");
        }
        None
    }

    /// Uma tecla. Retorna a leitura quando ela termina (Enter, ou Tab se
    /// configurado).
    pub fn key(&mut self, usage: u8, down: bool, at: Instant) -> Option<Burst> {
        if (0xE0..=0xE7).contains(&usage) {
            let bit = 1 << (usage - 0xE0);
            if down {
                self.mods |= bit;
            } else {
                self.mods &= !bit;
                // Soltou o Alt: emite o código Alt+numérico acumulado.
                if bit == LALT && !self.alt_digits.is_empty() {
                    let digits = std::mem::take(&mut self.alt_digits);
                    if let Some(c) = alt_code(&digits) {
                        return self.emit(c, at);
                    }
                }
            }
            return None;
        }
        if !down {
            return None;
        }
        let shift = self.mods & (LSHIFT | RSHIFT) != 0;
        let altgr = self.mods & RALT != 0;
        let ctrl = self.mods & (LCTRL | RCTRL) != 0 && !altgr;
        let alt = self.mods & LALT != 0;

        if alt && !ctrl {
            if let Some(d) = keypad_digit(usage) {
                self.alt_digits.push(char::from_digit(d, 10).unwrap());
                self.last = Some(at);
            }
            return None;
        }

        let out = map_key(self.layout, usage, shift, altgr);
        if ctrl {
            // Ctrl+letra = 0x01..0x1A; Ctrl+[ \ ] = ESC, FS, GS (o GS separa
            // campos em GS1-128/DataMatrix).
            let base = match map_key(self.layout, usage, false, false) {
                Out::Char(c) => c,
                _ => return self.action(out, at),
            };
            let code = match base {
                'a'..='z' => base as u32 - 'a' as u32 + 1,
                '[' => 0x1B,
                '\\' => 0x1C,
                ']' => 0x1D,
                '2' | '@' => 0, // NUL: ignorado
                '6' => 0x1E,
                '-' => 0x1F,
                _ => return None,
            };
            if code == 0 {
                return None;
            }
            return self.emit(char::from_u32(code).unwrap(), at);
        }
        self.action(out, at)
    }

    fn action(&mut self, out: Out, at: Instant) -> Option<Burst> {
        match out {
            Out::Char(c) => self.emit(c, at),
            Out::Dead(d) => {
                if let Some(prev) = self.dead.take() {
                    // Duas teclas mortas: a primeira sai sozinha.
                    self.push(prev, at);
                }
                self.dead = Some(d);
                if self.buf.is_empty() {
                    self.first = Some(at);
                }
                self.last = Some(at);
                None
            }
            Out::Enter => self.finish("enter"),
            Out::Tab if self.tab_ends => self.finish("tab"),
            Out::Tab => self.emit('\t', at),
            Out::Backspace => {
                self.buf.pop();
                self.last = Some(at);
                None
            }
            Out::None => None,
        }
    }

    fn emit(&mut self, c: char, at: Instant) -> Option<Burst> {
        if let Some(d) = self.dead.take() {
            match if c == ' ' { Some(d) } else { compose(d, c) } {
                Some(x) => self.push(x, at),
                None => {
                    self.push(d, at);
                    self.push(c, at);
                }
            }
        } else {
            self.push(c, at);
        }
        None
    }
}

/// Scancode do conjunto 1 (o que o Raw Input do Windows entrega) para usage
/// HID. `e0` é o prefixo das teclas estendidas.
pub fn set1_to_usage(make: u16, e0: bool) -> Option<u8> {
    if e0 {
        return Some(match make {
            0x1C => 0x58, // Enter do teclado numérico
            0x1D => 0xE4, // Ctrl direito
            0x35 => 0x54, // / do teclado numérico
            0x38 => 0xE6, // Alt direito (AltGr)
            0x5B => 0xE3,
            0x5C => 0xE7,
            0x47 => 0x4A,
            0x48 => 0x52,
            0x49 => 0x4B,
            0x4B => 0x50,
            0x4D => 0x4F,
            0x4F => 0x4D,
            0x50 => 0x51,
            0x51 => 0x4E,
            0x52 => 0x49,
            0x53 => 0x4C,
            _ => return None,
        });
    }
    #[rustfmt::skip]
    const T: [u8; 0x59] = [
        0x00, 0x29, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E, 0x2A, 0x2B, // 00-0F
        0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30, 0x28, 0xE0, 0x04, 0x16, // 10-1F
        0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x35, 0xE1, 0x31, 0x1D, 0x1B, 0x06, 0x19, // 20-2F
        0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0xE5, 0x55, 0xE2, 0x2C, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, // 30-3F
        0x3F, 0x40, 0x41, 0x42, 0x43, 0x53, 0x47, 0x5F, 0x60, 0x61, 0x56, 0x5C, 0x5D, 0x5E, 0x57, 0x59, // 40-4F
        0x5A, 0x5B, 0x62, 0x63, 0x00, 0x00, 0x64, 0x44, 0x45,                                           // 50-58
    ];
    match make {
        0x73 => Some(0x87), // ABNT2 /?
        0x7E => Some(0x85), // ABNT2 . do teclado numérico
        m if (m as usize) < T.len() && T[m as usize] != 0 => Some(T[m as usize]),
        _ => None,
    }
}

/// Converte relatórios de teclado HID (formato de boot: modificadores,
/// reservado, 6 teclas) em eventos de tecla, comparando com o anterior.
#[derive(Default)]
pub struct BootReport {
    mods: u8,
    keys: Vec<u8>,
}

impl BootReport {
    /// `has_report_id`: o primeiro byte é o número do relatório.
    pub fn diff(&mut self, report: &[u8], has_report_id: bool) -> Vec<(u8, bool)> {
        let r = if has_report_id { report.get(1..).unwrap_or(&[]) } else { report };
        if r.len() < 3 {
            return Vec::new();
        }
        let mods = r[0];
        let keys: Vec<u8> = r[2..r.len().min(8)].iter().copied().filter(|&k| k > 3).collect();
        // Rollover (todas as teclas = 1): ignora o relatório.
        if r[2..].iter().take(6).all(|&k| k == 1) {
            return Vec::new();
        }
        let mut ev = Vec::new();
        for bit in 0..8 {
            let (was, is) = (self.mods & (1 << bit) != 0, mods & (1 << bit) != 0);
            if is && !was {
                ev.push((0xE0 + bit, true));
            }
        }
        for &k in &self.keys {
            if !keys.contains(&k) {
                ev.push((k, false));
            }
        }
        for &k in &keys {
            if !self.keys.contains(&k) {
                ev.push((k, true));
            }
        }
        for bit in 0..8 {
            let (was, is) = (self.mods & (1 << bit) != 0, mods & (1 << bit) != 0);
            if was && !is {
                ev.push((0xE0 + bit, false));
            }
        }
        self.mods = mods;
        self.keys = keys;
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Digita `keys` (usage, shift) com 5 ms entre teclas.
    fn type_keys(d: &mut Decoder, keys: &[(u8, u8)]) -> Option<Burst> {
        let t0 = Instant::now();
        let mut out = None;
        for (i, &(usage, mods)) in keys.iter().enumerate() {
            let at = t0 + Duration::from_millis(5 * i as u64);
            for bit in 0..8 {
                if mods & (1 << bit) != 0 {
                    d.key(0xE0 + bit, true, at);
                }
            }
            if let Some(b) = d.key(usage, true, at) {
                out = Some(b);
            }
            d.key(usage, false, at);
            for bit in 0..8 {
                if mods & (1 << bit) != 0 {
                    if let Some(b) = d.key(0xE0 + bit, false, at) {
                        out = Some(b);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn us_text_with_enter() {
        let mut d = Decoder::new(Layout::Us, false);
        // "Ab1!" + Enter
        let b = type_keys(&mut d, &[(0x04, LSHIFT), (0x05, 0), (0x1E, 0), (0x1E, LSHIFT), (0x28, 0)]).unwrap();
        assert_eq!(b.text, "Ab1!");
        assert_eq!(b.terminator, "enter");
        assert_eq!(b.keys, 4);
        assert!((b.avg_gap_ms - 5.0).abs() < 0.01);
    }

    #[test]
    fn abnt2_symbols_and_dead_keys() {
        let mut d = Decoder::new(Layout::Abnt2, false);
        // ç  /  ;  ~a -> ã  ´e -> é  ~ espaço -> ~  : ?
        let b = type_keys(
            &mut d,
            &[(0x33, 0), (0x87, 0), (0x38, 0), (0x34, 0), (0x04, 0), (0x2F, 0), (0x08, 0), (0x34, 0), (0x2C, 0), (0x38, LSHIFT), (0x87, LSHIFT), (0x28, 0)],
        )
        .unwrap();
        assert_eq!(b.text, "ç/;ãé~:?");
        // O mesmo "/" num scanner US no layout US.
        let mut d = Decoder::new(Layout::Us, false);
        assert_eq!(type_keys(&mut d, &[(0x38, 0), (0x33, LSHIFT), (0x28, 0)]).unwrap().text, "/:");
    }

    #[test]
    fn ctrl_and_alt_codes() {
        let mut d = Decoder::new(Layout::Us, false);
        // ]C1 01 GS 10  (GS1 com Ctrl+])
        let b = type_keys(&mut d, &[(0x27, 0), (0x1E, 0), (0x30, LCTRL), (0x1E, 0), (0x27, 0), (0x28, 0)]).unwrap();
        assert_eq!(b.text, "01\u{1d}10");

        // Alt+0199 = Ç (1252), Alt+65 = A
        let mut d = Decoder::new(Layout::Us, false);
        let t = Instant::now();
        d.key(0xE2, true, t);
        for k in [0x62, 0x59, 0x61, 0x61] {
            d.key(k, true, t);
            d.key(k, false, t);
        }
        d.key(0xE2, false, t);
        d.key(0xE2, true, t);
        for k in [0x5E, 0x5D] {
            d.key(k, true, t);
        }
        d.key(0xE2, false, t);
        assert_eq!(d.key(0x28, true, t).unwrap().text, "ÇA");
    }

    #[test]
    fn idle_and_tab() {
        let mut d = Decoder::new(Layout::Us, true);
        assert_eq!(type_keys(&mut d, &[(0x04, 0), (0x2B, 0)]).unwrap().terminator, "tab");
        let mut d = Decoder::new(Layout::Us, false);
        assert!(type_keys(&mut d, &[(0x04, 0), (0x2B, 0), (0x05, 0)]).is_none());
        let later = Instant::now() + Duration::from_millis(500);
        let b = d.poll(later, 100).unwrap();
        assert_eq!(b.text, "a\tb");
        assert_eq!(b.terminator, "idle");
        assert!(d.poll(later, 100).is_none());
    }

    #[test]
    fn code_pages() {
        assert_eq!(CP437_HIGH.chars().count(), 128);
        assert_eq!(CP1252_80.chars().count(), 32);
        assert_eq!(alt_code("0128"), Some('€'));
        assert_eq!(alt_code("130"), Some('é'));
        assert_eq!(alt_code("0233"), Some('é'));
        assert_eq!(alt_code("0"), None);
    }

    #[test]
    fn scancodes() {
        assert_eq!(set1_to_usage(0x1E, false), Some(0x04)); // A
        assert_eq!(set1_to_usage(0x1C, false), Some(0x28)); // Enter
        assert_eq!(set1_to_usage(0x1C, true), Some(0x58));
        assert_eq!(set1_to_usage(0x2A, false), Some(0xE1));
        assert_eq!(set1_to_usage(0x52, false), Some(0x62)); // KP0
        assert_eq!(set1_to_usage(0x73, false), Some(0x87));
        assert_eq!(set1_to_usage(0x38, true), Some(0xE6));
        assert_eq!(set1_to_usage(0x10, false), Some(0x14)); // Q
        assert_eq!(set1_to_usage(0x32, false), Some(0x10)); // M
    }

    #[test]
    fn boot_reports() {
        let mut r = BootReport::default();
        // Shift + a
        assert_eq!(r.diff(&[0x02, 0, 0x04, 0, 0, 0, 0, 0], false), vec![(0xE1, true), (0x04, true)]);
        // solta tudo
        assert_eq!(r.diff(&[0, 0, 0, 0, 0, 0, 0, 0], false), vec![(0x04, false), (0xE1, false)]);
        // com número de relatório
        assert_eq!(r.diff(&[1, 0, 0, 0x05, 0, 0, 0, 0, 0], true), vec![(0x05, true)]);
    }
}
