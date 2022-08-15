//! Entity components stored together in contiguous arrays.
mod set;
mod archetype;
mod bundle;
mod query;

pub use set::*;
pub use archetype::*;
pub use bundle::*;
pub use query::*;
