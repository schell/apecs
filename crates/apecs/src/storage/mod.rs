//! Archetype or separated storage strategies.
//!
//! APECs provides both separated and archetypal storage strategies.
//! They are not meant to be used together at this time.
//! To read more about the difference between separated and archetypal storage
//! check out [this article](https://csherratt.github.io/blog/posts/specs-and-legion/).
use std::ops::{Deref, DerefMut};

use rayon::iter::{IntoParallelIterator, ParallelIterator};

mod archetype;
pub use archetype::*;

pub trait HasId {
    fn id(&self) -> usize;
}

impl<'a, T: HasId> HasId for &'a T {
    fn id(&self) -> usize {
        T::id(self)
    }
}

impl<'a, T: HasId> HasId for &'a mut T {
    fn id(&self) -> usize {
        T::id(self)
    }
}

/// For testing
impl<T> HasId for (usize, T) {
    fn id(&self) -> usize {
        self.0
    }
}

/// Component entries.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry<T> {
    pub(crate) value: T,
    /// The entity id.
    pub(crate) key: usize,
    /// The last time this component was changed.
    pub(crate) changed: u64,
    /// Whether this component was added the last time it was changed.
    pub(crate) added: bool,
}

impl<T> AsRef<T> for Entry<T> {
    fn as_ref(&self) -> &T {
        &self.value
    }
}

impl<T> HasId for Entry<T> {
    fn id(&self) -> usize {
        self.key
    }
}

impl<T> Deref for Entry<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> DerefMut for Entry<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.mark_changed();
        &mut self.value
    }
}

impl<T> Entry<T> {
    pub fn new(id: usize, value: T) -> Self {
        Entry {
            value,
            changed: crate::system::current_iteration(),
            added: true,
            key: id,
        }
    }

    pub fn id(&self) -> usize {
        self.key
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn set_value(&mut self, t: T) {
        self.mark_changed();
        self.value = t;
    }

    pub fn replace_value(&mut self, t: T) -> T {
        self.mark_changed();
        std::mem::replace(&mut self.value, t)
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn split(self) -> (usize, T) {
        (self.key, self.value)
    }

    #[inline]
    fn mark_changed(&mut self) {
        self.changed = crate::system::current_iteration();
        self.added = false;
    }

    pub fn has_changed_since(&self, iteration: u64) -> bool {
        self.changed >= iteration
    }

    pub fn was_added_since(&self, iteration: u64) -> bool {
        self.changed >= iteration && self.added
    }

    pub fn was_modified_since(&self, iteration: u64) -> bool {
        self.changed >= iteration && !self.added
    }

    pub fn last_changed(&self) -> u64 {
        self.changed
    }
}
