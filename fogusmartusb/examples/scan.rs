//! Lê códigos de um scanner: `cargo run --example scan -- [seletor] [opções JSON]`
//!   cargo run --example scan                    # primeiro scanner reconhecido
//!   cargo run --example scan -- keyboard        # qualquer teclado (filtro de rajada)
//!   cargo run --example scan -- COM5 '{"baud":115200}'
use std::time::Duration;

use fogusmartusb::scanner::{Opts, Scanner};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sel = args.first().map(String::as_str).unwrap_or("");
    let opts = args.get(1).map(|s| serde_json::from_str(s).expect("opções JSON")).unwrap_or(serde_json::json!({}));
    let mut s = match Opts::from_json(&opts).and_then(|o| Scanner::open(sel, o)) {
        Ok(s) => s,
        Err(e) => return eprintln!("erro: {e}"),
    };
    println!("lendo ({}, {}); 60s sem leitura encerra", s.mode, s.target);
    loop {
        match s.read(Some(Duration::from_secs(60))) {
            Ok(Some(scan)) => println!("{:#}", s.scan_json(scan)),
            Ok(None) => break,
            Err(e) => return eprintln!("erro: {e}"),
        }
    }
}
