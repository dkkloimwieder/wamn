//! Standard-node fixture importing only the typed HTTP connection capability.
//!
//! MVP outcome: minimum test coverage.

#![no_std]

extern crate alloc;

use alloc::string::ToString as _;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

struct BumpAllocator {
    next: AtomicUsize,
    bytes: UnsafeCell<[u8; 65_536]>,
}

// SAFETY: access to the backing bytes is partitioned by the atomic bump pointer.
#[expect(
    unsafe_code,
    reason = "The atomic bump pointer partitions access to the backing bytes."
)]
unsafe impl Sync for BumpAllocator {}

#[expect(
    unsafe_code,
    reason = "The atomic bump pointer reserves disjoint aligned ranges within the backing bytes."
)]
unsafe impl GlobalAlloc for BumpAllocator {
    #[expect(
        unsafe_code,
        reason = "The atomic bump pointer reserves an aligned range within the bounds of the backing bytes."
    )]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align_mask = layout.align() - 1;
        let mut current = self.next.load(Ordering::Relaxed);
        loop {
            let start = (current + align_mask) & !align_mask;
            let Some(end) = start.checked_add(layout.size()) else {
                return ptr::null_mut();
            };
            if end > 65_536 {
                return ptr::null_mut();
            }
            match self.next.compare_exchange_weak(
                current,
                end,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return unsafe { self.bytes.get().cast::<u8>().add(start) },
                Err(observed) => current = observed,
            }
        }
    }

    #[expect(
        unsafe_code,
        reason = "The bump allocator never reuses allocations, so deallocation is a no-op."
    )]
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    next: AtomicUsize::new(0),
    bytes: UnsafeCell::new([0; 65_536]),
};

#[expect(
    unsafe_code,
    reason = "The canonical ABI caller supplies valid allocation pointers, lengths, and alignments."
)]
#[unsafe(no_mangle)]
unsafe extern "C" fn cabi_realloc(
    old_ptr: *mut u8,
    old_len: usize,
    align: usize,
    new_len: usize,
) -> *mut u8 {
    let old_layout = unsafe { Layout::from_size_align_unchecked(old_len, align) };
    if new_len == 0 {
        if old_len != 0 {
            unsafe { alloc::alloc::dealloc(old_ptr, old_layout) };
        }
        return ptr::null_mut();
    }
    if old_len == 0 {
        let layout = unsafe { Layout::from_size_align_unchecked(new_len, align) };
        return unsafe { alloc::alloc::alloc(layout) };
    }
    unsafe { alloc::alloc::realloc(old_ptr, old_layout, new_len) }
}

#[allow(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:connection-http-standard-fixture/connection-http-standard@0.1.0",
        path: [
            "../../../../wit/wamn-connection",
            "wit",
        ],
        generate_all,
        async: [
            "import:wamn:connection/http@0.1.0#send",
            "export:wamn:connection-http-standard-fixture/node@0.1.0#run",
        ],
        std_feature,
    });
}

struct Component;

impl bindings::exports::wamn::connection_http_standard_fixture::node::Guest for Component {
    async fn run(_input: u32) -> u32 {
        let request = bindings::wamn::connection::http::Request {
            requirement: "standard-erp".to_string(),
            method: "POST".to_string(),
            path_and_query: "/receipts?source=standard".to_string(),
            headers: Vec::new(),
            body: None,
            // Frozen 0.1 ABI field: authored keys are not accepted.
            idempotency_key: None,
        };
        match bindings::wamn::connection::http::send(request).await {
            Ok(response) => u32::from(response.status),
            Err(_) => 0,
        }
    }
}

#[expect(
    unsafe_code,
    reason = "wit-bindgen generated bindings and exports contain unsafe FFI glue"
)]
mod component_exports {
    use super::{Component, bindings};

    bindings::export!(Component with_types_in bindings);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
