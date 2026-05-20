#[derive(Clone)]
pub struct GuestContext {
    host: Arc<dyn GuestHost>,
    scope_context: ScopeContext,
}

pub struct SharedMemoryHandle {
    context: GuestContext,
    descriptor: SharedMappingDescriptor,
    owns_region: bool,
}
