//! Entity components stored together in contiguous arrays.
mod archetype;
pub use archetype::*;
mod bundle;
pub use bundle::*;
mod all;
pub mod query;
pub use all::*;
