#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| oled_trust::frame::admin::fuzz_reply(data));
