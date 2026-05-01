//! Utility functions, type aliases, closures, and a `macro_rules!`
//! definition (the anti-regression for "macro definitions produce zero
//! symbols").
//!
//! Exercises:
//!   - Free functions
//!   - Type alias (`type Op = ...`)
//!   - Closure assigned to a `let` binding (closure body has no name; calls
//!     inside the closure resolve to the enclosing function as `from`)
//!   - `macro_rules!` definition — MUST yield zero Symbol records
//!   - `cfg(...)` attribute on a function (parser must not crash; the
//!     function still produces a Symbol)
//!
//! Symbol contract for `utils.rs` (asserted by `MANIFEST.md`):
//!   Functions: `add`, `mul`, `with_closure`, `cfg_gated_fn`, `unsafe_op` (5)
//!   Type aliases: `Op` (1)
//!   `my_macro` macro_rules!: 0 symbols (anti-regression)

pub type Op = fn(i32, i32) -> i32;

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn mul(a: i32, b: i32) -> i32 {
    a * b
}

/// Anti-regression fixture — `macro_rules!` definitions must NOT produce
/// any Symbol records. Invocations of macros DO produce Calls edges, but
/// the definition itself is silent. The corpus test asserts that no
/// Symbol named `my_macro` is emitted from this file.
#[macro_export]
macro_rules! my_macro {
    () => {
        ()
    };
    ($x:expr) => {
        $x
    };
}

pub fn with_closure(seed: i32) -> i32 {
    // Closure invocation produces a Calls edge from `with_closure` to `f`
    // (the binding name); `add` inside the closure body produces a Calls
    // edge from `with_closure` to `add` (closures are transparent for
    // enclosing-function-id resolution).
    let f = |x: i32| add(x, seed);
    f(10)
}

/// `#[cfg]`-attributed function — the parser does NOT evaluate cfg, so the
/// function still produces a single Symbol regardless of the host platform.
#[cfg(target_pointer_width = "64")]
pub fn cfg_gated_fn() -> u64 {
    0
}

/// Function containing an `unsafe` block; the parser must produce a single
/// symbol for the function and walk the unsafe body without crashing.
pub fn unsafe_op() -> i32 {
    let raw: *const i32 = &42 as *const i32;
    unsafe { *raw }
}
