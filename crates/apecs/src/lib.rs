//! *A*syncronous and *P*leasant *E*ntity *C*omponent *S*ystem
//!
#![feature(generic_associated_types)]
#![feature(vec_retain_mut)]
mod core;
pub use crate::core::*;

pub mod storage;
pub mod join;
pub mod world;

pub mod anyhow {
    pub use anyhow::*;
}

mod system;
pub use system::*;
