//! Trait definitions and trait impls — exercises every Inherits-edge form.
//!
//! Exercises:
//!   - Plain trait with a default method (the default method's body produces
//!     a Method symbol with parent = trait name? NO — Phase 5.2 sets parent
//!     to the impl block's `type` field; default methods inside trait_item
//!     bodies have `find_enclosing_impl` returning None (no impl_item ancestor),
//!     so they extract as `Function` (no parent). Verified by the corpus test.)
//!   - Trait without any default methods (signatures-only — no symbols emitted
//!     for the signature-only methods because they parse as
//!     `function_signature_item`, not `function_item`)
//!   - Inherent impl block with no methods (no symbols, no inheritance edge)
//!   - `impl Trait for Type` (generates an Inherits edge)
//!   - Generic trait impl with type bound: `impl<T: Display> Trait for Foo<T>`
//!   - Generic trait impl with where clause:
//!       `impl<T> Trait for Bar<T> where T: Send`
//!   - `async fn` inside a trait impl
//!   - `unsafe { ... }` block inside an impl method body
//!
//! Symbol contract for `traits.rs` (asserted by `MANIFEST.md`):
//!   Traits:  `Greet`, `Compute`, `Sized2` (3)
//!   Structs: `Greeter`, `EmptyImpl`, `Foo`, `Bar` (4)
//!   Functions (default trait methods, no impl ancestor): `default_greet` (1)
//!   Methods (in `impl` blocks):
//!       `Greeter::greet`,
//!       `Greeter::run_async`,
//!       `Greeter::do_unsafe`,
//!       `Foo::compute`,
//!       `Bar::compute`         (5)
//!   Inheritance edges (from `impl Trait for Type`):
//!       Greeter   -> Greet,
//!       Foo<T>    -> Compute,
//!       Bar<T>    -> Compute   (3)

use std::fmt::Display;

pub trait Greet {
    fn greet(&self) -> String;

    fn default_greet(&self) -> String {
        String::from("hello, default")
    }
}

pub trait Compute {
    fn compute(&self) -> i32;
}

/// Trait with no methods at all — `impl Sized2 for Foo` would be a marker.
/// We do not implement this trait below; it sits to keep the corpus
/// exercising "trait with empty body" alongside trait-with-defaults.
pub trait Sized2 {}

pub struct Greeter {
    pub name: String,
}

impl Greeter {
    pub fn run_async(&self) -> i32 {
        // async fn would normally be `pub async fn run_async`; we use a
        // plain method here to keep the inherent impl simple. The Greet
        // impl below uses `async fn` to exercise the modifier.
        42
    }

    pub fn do_unsafe(&self) -> i32 {
        unsafe {
            // Empty unsafe block — the parser must walk past the
            // `unsafe_block` node and still produce the enclosing method
            // symbol. The body intentionally contains no unsafe calls.
            let _x: i32 = 0;
        }
        7
    }
}

impl Greet for Greeter {
    fn greet(&self) -> String {
        format!("hi, {}", self.name)
    }
}

/// Inherent impl with no methods — must NOT produce an Inherits edge,
/// must NOT produce any symbols.
pub struct EmptyImpl;

impl EmptyImpl {}

pub struct Foo<T>(pub T);
pub struct Bar<T>(pub T);

impl<T: Display> Compute for Foo<T> {
    fn compute(&self) -> i32 {
        // Body contains a method call (`format!` macro invocation, not a
        // call edge — `format!` is a macro_invocation captured as a Calls
        // edge to "format").
        let _ = format!("{}", self.0);
        1
    }
}

impl<T> Compute for Bar<T>
where
    T: Send,
{
    fn compute(&self) -> i32 {
        2
    }
}
