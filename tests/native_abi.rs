// Standalone Rust-to-Zi2c linkage test, also runnable before Cargo downloads.
#[repr(C)]
struct Tool { name: *const u8, length: i64, kind: i32 }
unsafe extern "C" {
    fn xcoder_resolve_tool(name: *const u8, length: i64, catalog: *const Tool, count: i64) -> i64;
}
fn lookup(name: &[u8], names: &[&[u8]]) -> i64 {
    let catalog: Vec<_> = names.iter().map(|name| Tool { name: name.as_ptr(), length: name.len() as i64, kind: 1 }).collect();
    unsafe { xcoder_resolve_tool(name.as_ptr(), name.len() as i64, catalog.as_ptr(), catalog.len() as i64) }
}
#[test]
fn actual_ffi_exact_utf8_embedded_nul_and_duplicates() {
    let catalog: &[&[u8]] = &[b"shell", "文字工具".as_bytes(), b"a\0b", b"dup", b"dup"];
    assert_eq!(lookup(b"shell", catalog), 0);
    assert_eq!(lookup("文字工具".as_bytes(), catalog), 1);
    assert_eq!(lookup(b"a\0b", catalog), 2);
    assert_eq!(lookup(b"dup", catalog), -1);
    assert_eq!(lookup(b"shel", catalog), -1);
    assert_eq!(lookup(b"", catalog), -1);
    assert_eq!(lookup(b"missing", &[]), -1);
}
#[test]
fn real_ffi_is_reentrant() {
    let threads: Vec<_> = (0..8).map(|_| std::thread::spawn(|| {
        for _ in 0..10000 { assert_eq!(lookup(b"target", &[b"other", b"target"]), 1); }
    })).collect();
    for thread in threads { thread.join().unwrap(); }
}
#[test]
fn malformed_foreign_arguments_are_rejected() {
    unsafe {
        assert_eq!(xcoder_resolve_tool(std::ptr::null(), 5, std::ptr::null(), 1), -1);
        assert_eq!(xcoder_resolve_tool(b"x".as_ptr(), 1, std::ptr::null(), -1), -1);
        assert_eq!(xcoder_resolve_tool(b"x".as_ptr(), 1, std::ptr::null(), 1), -1);
    }
}
