//! Welcome to the *A*syncronous and *P*leasant *E*ntity *C*omponent *S*ystem 😊
//!
//! `apecs` is a flexible and well-performing entity-component-system that plays nice with async runtimes.
//!
//! It's best to start learning about `apecs` from the [`World`], but if you
//! just want some examples please check out the [readme](https://github.com/schell/apecs#readme)
#![allow(clippy::type_complexity)]

mod entity;
mod facade;
mod storage;
mod world;

pub use apecs_derive::Edges;
pub use entity::*;
pub use facade::{Facade, FacadeSchedule};
pub use moongraph::{
    end, err, graph, ok, Edges, Graph, GraphError, Move, NodeResults, TypeKey, TypeMap, View,
    ViewMut, NoDefault,
};
pub use storage::{
    Components, Entry, IsBundle, IsQuery, LazyComponents, Maybe, MaybeMut, MaybeRef, Mut, Query,
    QueryGuard, QueryIter, Ref, Without,
};
pub use world::{current_iteration, Parallelism, World};

#[cfg(doctest)]
pub mod doctest {
    #[doc = include_str!("../../../README.md")]
    pub struct ReadmeDoctests;
}
