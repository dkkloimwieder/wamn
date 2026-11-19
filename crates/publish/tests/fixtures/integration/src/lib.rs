//! A small integration artifact for build tests.

#[cfg(target_arch = "wasm32")]
extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod component {
    use alloc::string::String;
    wit_bindgen::generate!({
        path: "../../../../../wit/wamn-integration",
        inline: "package test:fixture; world client { import wamn:integration/bound@0.1.0; export wamn:integration/health@0.1.0; }",
        world: "test:fixture/client",
        std_feature,
        generate_all,
    });
    struct Fixture;
    impl exports::wamn::integration::health::Guest for Fixture {
        fn probe() -> Result<(), String> {
            let _call = wamn::integration::bound::begin()?;
            Ok(())
        }
    }
    export!(Fixture);
}

pub fn value() -> u32 {
    1
}
