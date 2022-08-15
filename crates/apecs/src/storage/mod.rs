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

pub struct Without<T>(pub T);

impl<'a, T, C> IntoParallelIterator for Without<&'a T>
where
    &'a T: IntoParallelIterator<Item = Option<C>>,
{
    type Iter =
        rayon::iter::Map<<&'a T as IntoParallelIterator>::Iter, fn(Option<C>) -> Option<()>>;

    type Item = Option<()>;

    fn into_par_iter(self) -> Self::Iter {
        self.0
            .into_par_iter()
            .map(|mitem| if mitem.is_none() { Some(()) } else { None })
    }
}

/// An iterator that wraps a storage iterator, producing values
/// for indicies that **are not** contained within the storage.
pub struct WithoutIter<T: Iterator> {
    iter: T,
    id: usize,
    next_id: usize,
}

impl<T, C> WithoutIter<T>
where
    C: HasId,
    T: Iterator<Item = C>,
{
    pub(crate) fn new(mut iter: T) -> Self {
        let next_id = iter.next().map(|e| e.id()).unwrap_or_else(|| usize::MAX);
        WithoutIter {
            iter,
            id: 0,
            next_id,
        }
    }
}

impl<T: Iterator, C> Iterator for WithoutIter<T>
where
    C: HasId,
    T: Iterator<Item = C>,
{
    type Item = Entry<()>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.id == self.next_id {
            self.next_id = self
                .iter
                .next()
                .map(|e| e.id())
                .unwrap_or_else(|| usize::MAX);
            self.id += 1;
        }

        let entry = Entry {
            value: (),
            key: self.id,
            changed: 0,
            added: false,
        };
        self.id += 1;
        Some(entry)
    }
}

impl<T, C> IntoIterator for Without<T>
where
    C: HasId,
    T: IntoIterator<Item = C>,
{
    type Item = Entry<()>;

    type IntoIter = WithoutIter<T::IntoIter>;

    fn into_iter(self) -> Self::IntoIter {
        WithoutIter::new(self.0.into_iter())
    }
}

pub struct ComponentIter<T>(T);

impl<A, B> Iterator for ComponentIter<(A, B)>
where
    A: Iterator,
    B: Iterator,
{
    type Item = (A::Item, B::Item);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        Some((self.0 .0.next()?, self.0 .1.next()?))
    }
}

impl<A, B, C> Iterator for ComponentIter<(A, B, C)>
where
    A: Iterator,
    B: Iterator,
    C: Iterator,
{
    type Item = (A::Item, B::Item, C::Item);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        Some((self.0 .0.next()?, self.0 .1.next()?, self.0 .2.next()?))
    }
}
