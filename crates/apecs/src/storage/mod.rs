//! Component storage.
//!
//! APECs provides an archetypal storage strategy.
//! To read more about the difference between separated and archetypal storage
//! check out [this article](https://csherratt.github.io/blog/posts/specs-and-legion/).
mod archetype;
pub use archetype::*;

pub type Components = ArchetypeSet;
