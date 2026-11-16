#![allow(dead_code)]
//! Callbacks do `fogu:ffi` (Fogu 0.7+: `ffi.callback`).
//!
//! Regras do fogu:ffi que este módulo respeita:
//! - um callback só pode ser chamado na thread do interpretador, *durante* uma
//!   chamada FFI feita por ele (de outra thread a chamada retorna 0 e é
//!   reportada no stderr). Por isso as threads da lib nunca chamam o Fogu:
//!   elas enfileiram, e as funções `*_dispatch` entregam na thread de quem chamou;
//! - um erro dentro do callback é fatal para o programa Fogu, então o lado
//!   Fogu só guarda os dados no callback e processa depois, fora do frame nativo.
//!
//! Dois tipos de callback:
//! - `Sink(ptr, len) -> i32`: recebe a saída de uma função (texto UTF-8 ou
//!   bytes). Registrado uma vez com `*_set_sink`; as funções que escrevem num
//!   buffer do chamador passam a entregar pelo sink quando chamadas com
//!   `buf = NULL, cap = 0` (sem "buffer pequeno, chame de novo").
//! - `EventFn(json, json_len, bin, bin_len) -> i32`: um evento (JSON) e, se
//!   houver, um payload binário ao lado. Retorne 0 para parar o lote.

use std::sync::atomic::{AtomicUsize, Ordering};

pub type Sink = extern "C" fn(*const u8, usize) -> i32;
pub type EventFn = extern "C" fn(*const u8, usize, *const u8, usize) -> i32;

static SINK: AtomicUsize = AtomicUsize::new(0);

/// Ponteiro válido para uma fatia vazia (nunca passa um ponteiro pendurado).
const EMPTY: &[u8] = b"\0";

fn ptr(data: &[u8]) -> *const u8 {
    if data.is_empty() { EMPTY.as_ptr() } else { data.as_ptr() }
}

pub fn set_sink(f: Option<Sink>) {
    SINK.store(f.map_or(0, |f| f as usize), Ordering::Release);
}

pub fn sink() -> Option<Sink> {
    match SINK.load(Ordering::Acquire) {
        0 => None,
        // SAFETY: só guardamos valores vindos de um `Sink` em `set_sink`.
        p => Some(unsafe { std::mem::transmute::<usize, Sink>(p) }),
    }
}

/// Entrega `data` ao sink registrado. false se não há sink.
pub fn to_sink(data: &[u8]) -> bool {
    match sink() {
        Some(f) => {
            f(ptr(data), data.len());
            true
        }
        None => false,
    }
}

/// Chama um `EventFn`. Retorna false se o callback pediu para parar.
pub fn event(f: EventFn, json: &str, bin: &[u8]) -> bool {
    f(ptr(json.as_bytes()), json.len(), ptr(bin), bin.len()) != 0
}

/// `write_out` com sink: `buf = NULL, cap = 0` e um sink registrado entregam
/// `data` pelo sink e retornam o tamanho; senão, `None` (use o buffer).
pub fn try_sink(buf: *const u8, cap: i64, data: &[u8]) -> Option<i64> {
    if buf.is_null() && cap == 0 && to_sink(data) { Some(data.len() as i64) } else { None }
}
