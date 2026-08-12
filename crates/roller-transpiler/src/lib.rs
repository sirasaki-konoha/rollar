//! Transpiler: converts Roller AST into C source code.

mod emit;

pub use emit::{TranspileError, emit_program};

/// The C runtime header content embedded in the binary.
const RUNTIME_H: &str = include_str!("roller-runtime.h");

/// Return the C runtime header content.
pub fn runtime_header() -> &'static str {
    RUNTIME_H
}

/// Default library search path (embedded in binary).
const DEFAULT_LIB: &str = include_str!("../../../lib/gcc.roller");
const DEFAULT_LIB_CLANG: &str = include_str!("../../../lib/clang.roller");
const DEFAULT_LIB_ZIG: &str = include_str!("../../../lib/zig.roller");

/// Return the default library source for a module name, if available.
pub fn default_lib_source(name: &str) -> Option<&'static str> {
    match name {
        "gcc" => Some(DEFAULT_LIB),
        "clang" => Some(DEFAULT_LIB_CLANG),
        "zig" => Some(DEFAULT_LIB_ZIG),
        _ => None,
    }
}
