//! Shared allocation and panic floor for reusable `no_std` palette components.

#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

/// Canonical ABI allocator entry point required by exported component strings.
///
/// # Safety
///
/// A non-null `old_pointer` must denote an allocation of `old_length` bytes
/// with `alignment`; these are the canonical ABI realloc preconditions.
#[unsafe(no_mangle)]
unsafe extern "C" fn cabi_realloc(
    old_pointer: *mut u8,
    old_length: usize,
    alignment: usize,
    new_length: usize,
) -> *mut u8 {
    use alloc::alloc::{Layout, alloc, dealloc, realloc};

    if new_length == 0 {
        if old_length != 0 {
            // SAFETY: upheld by the caller's canonical ABI contract.
            let layout = unsafe { Layout::from_size_align_unchecked(old_length, alignment) };
            // SAFETY: `old_pointer` and `layout` describe the caller's allocation.
            unsafe { dealloc(old_pointer, layout) };
        }
        return core::ptr::null_mut();
    }
    if old_length == 0 {
        // SAFETY: canonical ABI alignments are valid powers of two.
        let layout = unsafe { Layout::from_size_align_unchecked(new_length, alignment) };
        // SAFETY: `layout` is valid by the statement above.
        return unsafe { alloc(layout) };
    }
    // SAFETY: upheld by the caller's canonical ABI contract.
    let layout = unsafe { Layout::from_size_align_unchecked(old_length, alignment) };
    // SAFETY: `old_pointer` and `layout` describe the caller's allocation.
    unsafe { realloc(old_pointer, layout, new_length) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    #[cfg(target_arch = "wasm32")]
    core::arch::wasm32::unreachable();

    #[cfg(not(target_arch = "wasm32"))]
    loop {
        core::hint::spin_loop();
    }
}

/// Compiler comparison intrinsic needed by `alloc`-backed JSON without `std`.
///
/// # Safety
///
/// The compiler-generated caller must provide two readable regions of at least
/// `length` bytes. This has the same contract as C `memcmp`.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(left: *const u8, right: *const u8, length: usize) -> i32 {
    for offset in 0..length {
        // SAFETY: the function's C ABI contract makes both regions readable
        // for every offset strictly below `length`.
        let left = unsafe { left.add(offset).read() };
        // SAFETY: same contract and bound as the left-hand read above.
        let right = unsafe { right.add(offset).read() };
        if left != right {
            return i32::from(left) - i32::from(right);
        }
    }
    0
}
