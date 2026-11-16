//! Lista os aparelhos USB reconhecidos: `cargo run --example list [-- --json]`
fn main() {
    let json = std::env::args().any(|a| a == "--json");
    match fogusmartusb::enumerate::list(false) {
        Ok(devs) => {
            for d in devs {
                if json {
                    println!("{:#}", d.to_json());
                    continue;
                }
                let c = &d.class;
                println!("{:<12} {} {:<20} {} [{}; {}]", d.id, d.vid_pid(), c.kind, c.name, c.confidence, c.source);
                if !c.modes.is_empty() {
                    println!("{:<12} modos de leitura: {}", "", c.modes.join(", "));
                }
                for p in &d.raw.ports {
                    println!("{:<12} porta: {p}", "");
                }
                for r in &d.raw.readers {
                    println!("{:<12} leitor PC/SC: {r}", "");
                }
            }
        }
        Err(e) => eprintln!("erro: {e}"),
    }
}
