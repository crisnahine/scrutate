//! Arbitrary bytes as an .npy file: the reader must reject or read them, never panic.
#![no_main]
use libfuzzer_sys::fuzz_target;
use scrutate::npy::Npy;

fuzz_target!(|data: &[u8]| {
    if let Ok(a) = Npy::from_bytes(data.to_vec()) {
        let _ = a.len();
        let _ = a.to_vec::<i64>();
        let _ = a.to_vec::<f32>();
        let _ = a.to_vec::<u32>();
        let _ = a.to_strings();
        let _ = a.view::<u8>();
    }
});
