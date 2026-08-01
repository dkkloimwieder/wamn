//! Full first-party Postgres-class specialization fixture.

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
    world: "postgres-class",
    path: "../capability-class-wit",
});

struct Component;

impl exports::wamn::capability_class::postgres_query::Guest for Component {
    fn run(input: u32) -> u32 {
        wamn::capability_class::postgres_capability::query(input).wrapping_add(3)
    }
}

impl exports::wamn::capability_class::postgres_write::Guest for Component {
    fn run(input: u32) -> u32 {
        wamn::capability_class::postgres_capability::query(input).wrapping_add(303)
    }
}

export!(Component);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
