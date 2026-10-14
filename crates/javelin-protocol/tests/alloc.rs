//! Measures heap allocation at the codec boundary with a counting allocator.
//! This file is its own test binary and runs its checks sequentially.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    io::Cursor,
    sync::atomic::{AtomicUsize, Ordering},
};

use javelin::ObjectId;
use javelin_protocol::{Message, codec, limits::*};

struct Counting;
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocated_by<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATED.load(Ordering::SeqCst);
    let value = f();
    (value, ALLOCATED.load(Ordering::SeqCst) - before)
}

#[test]
fn oversize_input_and_output_are_refused_before_large_allocations() {
    let id = ObjectId::parse(&"ab".repeat(32)).unwrap();
    let blobs = Message::Blobs(vec![(id, vec![7; MAX_BLOB]); 3]);
    let mut sink = Vec::new();
    let (result, bytes) = allocated_by(|| codec::write_frame(&mut sink, &blobs));
    assert_eq!(result.unwrap_err().code, "limit_exceeded");
    assert!(
        bytes < 4096,
        "encoder allocated {bytes} bytes before refusing"
    );
    assert!(sink.is_empty());

    // A valid empty Want padded past the limit: only the entry check reports a limit.
    let mut body = vec![0x10, 0, 0, 0, 0];
    body.resize(MAX_FRAME + 1, 0);
    let (result, bytes) = allocated_by(|| codec::decode(&body));
    assert_eq!(result.unwrap_err().code, "limit_exceeded");
    assert!(
        bytes < 4096,
        "decoder allocated {bytes} bytes before refusing"
    );

    let mut header = Cursor::new(u32::MAX.to_be_bytes().to_vec());
    let (result, bytes) = allocated_by(|| codec::read_frame(&mut header));
    assert_eq!(result.unwrap_err().code, "limit_exceeded");
    assert!(
        bytes < 4096,
        "reader allocated {bytes} bytes before refusing"
    );

    let mut count = vec![0x10];
    count.extend(((MAX_WANT + 1) as u32).to_be_bytes());
    let (result, bytes) = allocated_by(|| codec::decode(&count));
    assert_eq!(result.unwrap_err().code, "limit_exceeded");
    assert!(bytes < 4096, "count check allocated {bytes} bytes");
}
