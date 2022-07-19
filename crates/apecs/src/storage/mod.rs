//! Archetype or separated storage strategies.
//!
//! APECs provides both separated and archetypal storage strategies.
//! They are not meant to be used together at this time.
//! To read more about the difference between separated and archetypal storage
//! check out [this article](https://csherratt.github.io/blog/posts/specs-and-legion/).
pub mod archetype;
pub mod separate;

use std::ops::{Deref, DerefMut};

use rayon::iter::{IntoParallelIterator, ParallelIterator};
use tuple_list::TupleList;

pub trait HasId {
    fn id(&self) -> usize;
}

impl<T> HasId for Entry<T> {
    fn id(&self) -> usize {
        Entry::id(self)
    }
}

impl<T> HasId for &Entry<T> {
    fn id(&self) -> usize {
        Entry::id(self)
    }
}

impl<T> HasId for &mut Entry<T> {
    fn id(&self) -> usize {
        Entry::id(self)
    }
}

impl<T> HasId for (usize, T) {
    fn id(&self) -> usize {
        self.0
    }
}

impl HasId for usize {
    fn id(&self) -> usize {
        *self
    }
}

pub trait IsEntry {
    type Value<'a>
    where
        Self: 'a;

    fn id(&self) -> usize;

    fn value(&self) -> Self::Value<'_>;
}

impl IsEntry for usize {
    type Value<'a> = usize;

    fn id(&self) -> usize {
        *self
    }

    fn value(&self) -> Self {
        *self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry<T> {
    pub(crate) key: usize,
    pub(crate) value: T,
    changed: u64,
    added: bool,
}

impl<T> IsEntry for Entry<T> {
    type Value<'a> = &'a T
        where T: 'a;

    fn id(&self) -> usize {
        self.id()
    }

    fn value(&self) -> &T {
        Entry::value(self)
    }
}

impl<'a, T> IsEntry for &'a Entry<T> {
    type Value<'b> = &'b T
        where 'a: 'b;

    fn id(&self) -> usize {
        self.key
    }

    fn value(&self) -> &T {
        Entry::value(self)
    }
}

impl<'a, T> IsEntry for &'a mut Entry<T> {
    type Value<'b> = &'b T
        where 'a: 'b;

    fn id(&self) -> usize {
        self.key
    }

    fn value(&self) -> &T {
        Entry::value(self)
    }
}

impl<T:IsEntry> IsEntry for (T, ()) {
    type Value<'t> = (T::Value<'t>, ())
        where T: 't;

    fn id(&self) -> usize {
        self.0.id()
    }

    fn value(&self) -> Self::Value<'_> {
        (self.0.value(), ())
    }
}

impl<Head, Next, Tail> IsEntry for (Head, (Next, Tail))
where
    Head: IsEntry,
    (Next, Tail): IsEntry + TupleList,
{
    type Value<'a> = (Head::Value<'a>, <(Next, Tail) as IsEntry>::Value<'a>)
    where
        Self: 'a;

    fn id(&self) -> usize {
        self.0.id()
    }

    fn value(&self) -> Self::Value<'_> {
        (self.0.value(), self.1.value())
    }
}

impl<T> Deref for Entry<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> DerefMut for Entry<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.mark_changed();
        &mut self.value
    }
}

impl<T> Entry<T> {
    pub fn new(id: usize, value: T) -> Self {
        Entry {
            key: id,
            value,
            changed: crate::system::current_iteration(),
            added: true,
        }
    }

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

    pub fn id(&self) -> usize {
        self.key
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn split(self) -> (usize, T) {
        (self.key, self.value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Maybe<T> {
    pub key: usize,
    pub inner: Option<T>,
}

impl<T> HasId for Maybe<T> {
    fn id(&self) -> usize {
        self.key
    }
}

impl<T: IsEntry> IsEntry for Maybe<T> {
    type Value<'a> = Option<T::Value<'a>>
        where T: 'a;

    fn id(&self) -> usize {
        self.key
    }

    fn value(&self) -> Option<T::Value<'_>> {
        self.inner.as_ref().map(IsEntry::value)
    }
}

pub struct MaybeIter<C: IsEntry, T: Iterator<Item = C>> {
    iter: T,
    id: usize,
    next_id: usize,
    next_entry: Option<C>,
}

impl<C, T> MaybeIter<C, T>
where
    C: IsEntry,
    T: Iterator<Item = C>,
{
    pub(crate) fn new(mut iter: T) -> Self {
        let next_entry = iter.next();
        MaybeIter {
            iter,
            id: 0,
            next_id: next_entry.as_ref().map(|e| e.id()).unwrap_or(usize::MAX),
            next_entry,
        }
    }
}

impl<C, T> Iterator for MaybeIter<C, T>
where
    C: IsEntry,
    T: Iterator<Item = C>,
{
    type Item = Maybe<C>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = if self.next_id == self.id {
            if let Some(entry) = self.next_entry.take() {
                self.next_entry = self.iter.next();
                self.next_id = self
                    .next_entry
                    .as_ref()
                    .map(|e| e.id())
                    .unwrap_or_else(|| usize::MAX);
                Some(entry)
            } else {
                None
            }
        } else {
            None
        };
        let maybe = Maybe {
            key: self.id,
            inner: entry,
        };
        self.id += 1;
        Some(maybe)
    }
}

pub struct MaybeParIter<T>(T);

impl<T: IntoParallelIterator> IntoParallelIterator for MaybeParIter<T> {
    type Iter = rayon::iter::Map<T::Iter, fn(T::Item) -> Option<T::Item>>;

    type Item = Option<T::Item>;

    fn into_par_iter(self) -> Self::Iter {
        self.0.into_par_iter().map(Option::Some)
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
    C: IsEntry,
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
    C: IsEntry,
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
            key: self.id,
            value: (),
            changed: 0,
            added: false,
        };
        self.id += 1;
        Some(entry)
    }
}

impl<T, C> IntoIterator for Without<T>
where
    C: IsEntry,
    T: IntoIterator<Item = C>,
{
    type Item = Entry<()>;

    type IntoIter = WithoutIter<T::IntoIter>;

    fn into_iter(self) -> Self::IntoIter {
        WithoutIter::new(self.0.into_iter())
    }
}
