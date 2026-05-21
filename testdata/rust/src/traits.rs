//! Trait definitions and trait impls — exercises every Inherits-edge form.
//!
//! Exercises:
//!   - Plain trait with a default method (post-Task-1.4: the default
//!     method's body produces a Method symbol with parent = trait name,
//!     because the dispatch's NearestDefAncestor::Trait branch fires when
//!     the function's nearest definition ancestor is `trait_item`).
//!   - Trait with abstract method signatures (`fn f(&self);` no-body
//!     declarations parsed as `function_signature_item`): post-Task-1.4
//!     these are extracted as Method symbols with parent = trait name —
//!     a deliberate, scoped exception to the "forward declarations
//!     excluded" cross-language invariant.
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
//!   Methods (post-Task-1.4):
//!       Trait methods (default or abstract, parent = trait):
//!           `Greet::greet`              (abstract signature)
//!           `Greet::default_greet`      (default method body)
//!           `Compute::compute`          (abstract signature)
//!       Impl methods (parent = implementing type):
//!           `Greeter::greet`,
//!           `Greeter::run_async`,
//!           `Greeter::do_unsafe`,
//!           `Foo<T>::compute`,
//!           `Bar<T>::compute`           (5)
//!       Total methods: 8
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
