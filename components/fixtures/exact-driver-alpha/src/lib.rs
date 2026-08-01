//! Socket for an observed fleet bundle selecting only `alpha`.

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
    world: "alpha-driver",
    path: "../exact-bundle-wit",
    generate_all,
});

use wamn::exact_bundle::alpha::run as run_alpha;

struct Component;

impl Guest for Component {
    fn run_flow(input: u32) -> u32 {
        run_alpha(input)
    }
}

export!(Component);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
