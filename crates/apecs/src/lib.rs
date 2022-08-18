//! *A*syncronous and *P*leasant *E*ntity *C*omponent *S*ystem
#![feature(generic_associated_types)]
#![allow(clippy::type_complexity)]
mod core;
pub mod resource_manager;
pub mod schedule;
pub mod system;
pub mod storage;
pub mod plugins;
pub mod world;
pub mod anyhow {
    pub use anyhow::*;
}

pub use crate::core::*;

pub use rustc_hash::FxHashMap;

#[doc = include_str!("../../../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
