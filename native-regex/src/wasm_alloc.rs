//! C allocations share Rust's Wasm heap. A separate libc heap would overlap it.
use std::{
    alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc as resize},
    ffi::c_void,
};
const HEADER: usize = 16;
fn layout(size: usize) -> Option<Layout> {
    Layout::from_size_align(size.checked_add(HEADER)?, 16).ok()
}
unsafe fn allocate(size: usize, zero: bool) -> *mut c_void {
    let Some(layout) = layout(size) else {
        return std::ptr::null_mut();
    };
    let base = unsafe {
        if zero {
            alloc_zeroed(layout)
        } else {
            alloc(layout)
        }
    };
    if base.is_null() {
        return base.cast();
    }
    unsafe {
        base.cast::<usize>().write(size);
        base.add(HEADER).cast()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    unsafe { allocate(size, false) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    count
        .checked_mul(size)
        .map_or(std::ptr::null_mut(), |size| unsafe { allocate(size, true) })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(pointer: *mut c_void) {
    if pointer.is_null() {
        return;
    }
    // All libc/GNU allocations resolve to these symbols. The private header
    // records the exact Rust Layout; C callers never own Rust Vec allocations.
    unsafe {
        let base = pointer.cast::<u8>().sub(HEADER);
        let size = base.cast::<usize>().read();
        dealloc(base, layout(size).expect("Corrupt C allocation header"));
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
    if pointer.is_null() {
        return unsafe { malloc(size) };
    }
    if size == 0 {
        unsafe { free(pointer) };
        return std::ptr::null_mut();
    }
    let Some(next) = layout(size) else {
        return std::ptr::null_mut();
    };
    unsafe {
        let base = pointer.cast::<u8>().sub(HEADER);
        let old = base.cast::<usize>().read();
        let changed = resize(
            base,
            layout(old).expect("Corrupt C allocation header"),
            next.size(),
        );
        if changed.is_null() {
            return std::ptr::null_mut();
        }
        changed.cast::<usize>().write(size);
        changed.add(HEADER).cast()
    }
}
