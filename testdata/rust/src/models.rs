//! Model types: structs (named, tuple, unit), enums (all variant shapes),
//! and a nested struct inside a module.
//!
//! Exercises:
//!   - `#[derive(...)]` attributes (must NOT produce call edges per the
//!     parser limitations)
//!   - Tuple struct, named-field struct, unit struct
//!   - Enum with unit / tuple / struct variants
//!   - Nested `mod` populating namespace on inner symbols
//!
//! Symbol contract for `models.rs` (asserted by `MANIFEST.md`):
//!   - 4 structs:  `Vec2`, `RGB`, `Marker`, `Inner` (the last has namespace `nested`)
//!   - 2 enums:   `Shape`, `Status`
//!   - 2 methods: `Vec2::new`, `Vec2::magnitude_squared`
//!   - 1 function: `nested_helper` (with namespace `nested`)
//!   - Note: `nested` is a `mod_item` and is intentionally NOT emitted as a Symbol.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vec2 {
    pub x: i32,
    pub y: i32,
}

impl Vec2 {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    pub fn magnitude_squared(self) -> i32 {
        self.x * self.x + self.y * self.y
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RGB(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy)]
pub struct Marker;

pub enum Shape {
    Circle(f32),
    Rectangle { width: f32, height: f32 },
    Empty,
}

#[derive(Debug)]
pub enum Status {
    Ready,
    Pending(u32),
    Failed { code: i32, message: String },
}

pub mod nested {
    //! Nested module — symbols inside should carry the `nested` namespace.

    pub struct Inner {
        pub value: i32,
    }

    pub fn nested_helper() -> i32 {
        42
    }
}
