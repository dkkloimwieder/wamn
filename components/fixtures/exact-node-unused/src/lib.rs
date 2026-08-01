//! Unselected implementation used to prove exact-node exclusion.

#![no_std]

use core::alloc::{GlobalAlloc, Layout};

struct NoAlloc;

// SAFETY: these scalar-only fixtures never allocate. Returning null is permitted for
// allocation failure; `dealloc` is unreachable because this allocator returns no pointer.
unsafe impl GlobalAlloc for NoAlloc {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static ALLOCATOR: NoAlloc = NoAlloc;

wit_bindgen::generate!({
    world: "unused-node",
    path: "../exact-bundle-wit",
});

struct Component;

impl exports::wamn::exact_bundle::unused::Guest for Component {
    fn run(input: u32) -> u32 {
        input.wrapping_add(0xDEAD_BEEF)
    }
}

export!(Component);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
