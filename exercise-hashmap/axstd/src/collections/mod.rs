//! Collection types.
//!
//! This module re-exports all collection types from `alloc::collections` and
//! additionally provides `HashMap` and `HashSet` from the `hashbrown` crate.

#[cfg(feature = "alloc")]
pub use alloc::collections::*;

#[cfg(feature = "alloc")]
pub use hashbrown::{HashMap, HashSet};
