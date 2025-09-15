pub const WRITE_MASK: u64 = 1 << 63;
pub const TIMEOUT_MASK: u64 = 1 << 62;
pub const TIMEOUT_CLEAR_MASK: u64 = WRITE_MASK | TIMEOUT_MASK;
