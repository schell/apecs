//! Archetype or separated storage strategies.
//!
//! APECs provides both separated and archetypal storage strategies.
//! They are not meant to be used together at this time.
//! To read more about the difference between separated and archetypal storage
//! check out [this article](https://csherratt.github.io/blog/posts/specs-and-legion/).
pub mod archetype;
pub mod separate;

use std::ops::{Deref, DerefMut};

use tuple_list::TupleList;

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
