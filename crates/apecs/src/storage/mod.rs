//! Archetype or separated storage strategies.
//!
//! APECs provides both separated and archetypal storage strategies.
//! They are not meant to be used together at this time.
//! To read more about the difference between separated and archetypal storage
//! check out [this article](https://csherratt.github.io/blog/posts/specs-and-legion/).
pub mod archetype;
pub mod separated;
pub mod tracking;

use std::ops::{Deref, DerefMut};

use rayon::iter::{IntoParallelIterator, ParallelIterator};

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

/// Information about an entity.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityInfo {
    /// The entity id.
    pub key: usize,
    /// The last time this component was changed.
    pub changed: u64,
    /// Whether this component was added the last time it was changed.
    pub added: bool,
}

impl EntityInfo {
    pub fn new(id: usize) -> EntityInfo {
        EntityInfo {
            key: id,
            changed: crate::system::current_iteration(),
            added: true,
        }
    }
}

impl HasId for EntityInfo {
    fn id(&self) -> usize {
        self.key
    }
}

pub trait HasEntityInfo {
    fn info(&self) -> &EntityInfo;

    fn has_changed_since(&self, iteration: u64) -> bool {
        self.info().changed >= iteration
    }

    fn was_added_since(&self, iteration: u64) -> bool {
        self.info().changed >= iteration && self.info().added
    }

    fn was_modified_since(&self, iteration: u64) -> bool {
        self.info().changed >= iteration && !self.info().added
    }

    fn last_changed(&self) -> u64 {
        self.info().changed
    }
}

impl<T: HasEntityInfo> HasEntityInfo for &T {
    fn info(&self) -> &EntityInfo {
        T::info(self)
    }
}

impl HasEntityInfo for EntityInfo {
    fn info(&self) -> &EntityInfo {
        self
    }
}

pub trait HasEntityInfoMut {
    fn info_mut(&mut self) -> &mut EntityInfo;

    fn mark_changed(&mut self) {
        let info = self.info_mut();
        info.changed = crate::system::current_iteration();
        info.added = false;
    }
}

impl HasEntityInfoMut for EntityInfo {
    fn info_mut(&mut self) -> &mut EntityInfo {
        self
    }
}

/// Owned component entries.
///
/// "Owned" here means that this is not a reference.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry<T> {
    pub(crate) value: T,
    pub(crate) info: EntityInfo,
}

impl<T> AsRef<T> for Entry<T> {
    fn as_ref(&self) -> &T {
        &self.value
    }
}

impl<T> HasId for Entry<T> {
    fn id(&self) -> usize {
        self.info.key
    }
}

impl<T> HasEntityInfo for Entry<T> {
    fn info(&self) -> &EntityInfo {
        &self.info
    }
}

impl<T> HasEntityInfoMut for Entry<T> {
    fn info_mut(&mut self) -> &mut EntityInfo {
        &mut self.info
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
        self.info.mark_changed();
        &mut self.value
    }
}

impl<T> Entry<T> {
    pub fn new(id: usize, value: T) -> Self {
        Entry {
            value,
            info: EntityInfo::new(id),
        }
    }

    pub fn id(&self) -> usize {
        self.info.key
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn set_value(&mut self, t: T) {
        self.info.mark_changed();
        self.value = t;
    }

    pub fn replace_value(&mut self, t: T) -> T {
        self.info.mark_changed();
        std::mem::replace(&mut self.value, t)
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn split(self) -> (usize, T) {
        (self.info.key, self.value)
    }
}

/// A reference to a component stored in an archetype.
pub struct Ref<'a, T: 'static>(&'a EntityInfo, &'a T);

impl<'a, T> HasId for Ref<'a, T> {
    fn id(&self) -> usize {
        self.0.key
    }
}

impl<'a, T> HasEntityInfo for Ref<'a, T> {
    fn info(&self) -> &EntityInfo {
        &self.0
    }
}

impl<'a, T> Deref for Ref<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.1
    }
}

impl<'a, T> AsRef<T> for Ref<'a, T> {
    fn as_ref(&self) -> &T {
        self.1
    }
}

impl<'a, T: 'static> Ref<'a, T> {
    pub fn id(&self) -> usize {
        self.0.key
    }
}

/// A mutable reference to a component stored in an archetype.
pub struct Mut<'a, T: 'static>(&'a mut EntityInfo, &'a mut T);

impl<'a, T> HasId for Mut<'a, T> {
    fn id(&self) -> usize {
        self.0.key
    }
}

impl<'a, T> HasEntityInfo for Mut<'a, T> {
    fn info(&self) -> &EntityInfo {
        &self.0
    }
}

impl<'a, T> HasEntityInfoMut for Mut<'a, T> {
    fn info_mut(&mut self) -> &mut EntityInfo {
        &mut self.0
    }
}

impl<'a, T> Deref for Mut<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.1
    }
}

impl<'a, T> DerefMut for Mut<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.mark_changed();
        self.1
    }
}

impl<'a, T> AsRef<T> for Mut<'a, T> {
    fn as_ref(&self) -> &T {
        self.1
    }
}

impl<'a, T> AsMut<T> for Mut<'a, T> {
    fn as_mut(&mut self) -> &mut T {
        self.1
    }
}

impl<'a, T: 'static> Mut<'a, T> {
    pub fn id(&self) -> usize {
        self.0.key
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

pub struct MaybeIter<C: HasId, T: Iterator<Item = C>> {
    iter: T,
    id: usize,
    next_id: usize,
    next_entry: Option<C>,
}

impl<C, T> MaybeIter<C, T>
where
    C: HasId,
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
    C: HasId,
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
            info: EntityInfo {
                key: self.id,
                changed: 0,
                added: false,
            },
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
