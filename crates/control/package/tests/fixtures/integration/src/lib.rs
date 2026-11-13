//! A small integration artifact for build and publication tests.

#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
extern crate alloc;

#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}

#[cfg(target_arch = "wasm32")]
mod component {
    use alloc::string::String;
    wit_bindgen::generate!({
        inline: "package test:fixture; world client { export wamn:integration/health@0.1.0; } package wamn:integration@0.1.0 { interface health { probe: func() -> result<_, string>; } }",
        world: "test:fixture/client",
        std_feature,
    });
    struct Fixture;
    impl exports::wamn::integration::health::Guest for Fixture {
        fn probe() -> Result<(), String> {
            Ok(())
        }
    }
    export!(Fixture);
}

pub fn value() -> u32 {
    1
}
