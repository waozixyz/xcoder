use codex_code_mode_protocol::ToolDefinition;

#[repr(C)]
struct Tool<'a> {
    name: *const u8,
    length: i64,
    kind: i32,
    lifetime: std::marker::PhantomData<&'a str>,
}

unsafe extern "C" {
    fn xcoder_resolve_tool(name: *const u8, length: i64, tools: *const Tool<'_>, count: i64)
    -> i64;
}

pub(crate) struct Catalog<'a> {
    definitions: &'a [ToolDefinition],
    entries: Vec<Tool<'a>>,
}

// SAFETY: entries point only into immutable borrowed strings. The C ABI retains
// nothing and performs no mutation. The borrow outlives every native lookup.
unsafe impl Send for Catalog<'_> {}
unsafe impl Sync for Catalog<'_> {}

impl<'a> Catalog<'a> {
    pub(crate) fn new(definitions: &'a [ToolDefinition]) -> Self {
        let entries = definitions
            .iter()
            .map(|tool| Tool {
                name: tool.name.as_ptr(),
                length: tool.name.len() as i64,
                kind: 0,
                lifetime: std::marker::PhantomData,
            })
            .collect();
        Self {
            definitions,
            entries,
        }
    }

    pub(crate) fn resolve(&self, name: &str) -> Option<&'a ToolDefinition> {
        // SAFETY: all pointers borrow immutable slices held for this call. Ziran
        // checks lengths and returns an index; it retains no pointers or ownership.
        let index = unsafe {
            xcoder_resolve_tool(
                name.as_ptr(),
                name.len() as i64,
                self.entries.as_ptr(),
                self.entries.len() as i64,
            )
        };
        usize::try_from(index)
            .ok()
            .and_then(|index| self.definitions.get(index))
    }
}
