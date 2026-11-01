#![no_main]

use std::io::{Cursor, Read};

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(decoder) = pakk::pgzip::decoder(Cursor::new(data.to_vec())) {
        let _ = decoder.take(64 << 20).read_to_end(&mut Vec::new());
    }
});
