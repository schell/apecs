//! *A*syncronous and *P*leasant *E*ntity *C*omponent *S*ystem
//!
#![feature(generic_associated_types)]
#![feature(vec_retain_mut)]
#![allow(clippy::type_complexity)]
mod core;
pub mod system;
pub mod entities;
pub mod join;
pub mod schedule;
pub mod storage;
pub mod world;
pub mod plugins;
pub mod anyhow {
    pub use anyhow::*;
}

pub use crate::core::*;
