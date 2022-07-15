//! Entity component storage traits.
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use std::ops::{Deref, DerefMut};

// mod range_map;
// pub use range_map::*;
// mod tracking;
// pub use tracking::*;
mod vec;
pub use vec::*;
pub mod join;
pub use join::*;

use crate::{IsResource, Read, ReadExpect, Write, WriteExpect};

use super::{Entry, IsEntry};

pub mod bitset {
    pub use hibitset::*;
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

///// A storage that can only be read from
// pub trait CanReadStorage: Send + Sync {
//    type Component: Send + Sync;
//
//    type Iter<'a>: Iterator<Item = &'a Entry<Self::Component>>
//    where
//        Self: 'a;
//
//    type ParIter<'a>: IndexedParallelIterator<Item = Option<&'a
// Entry<Self::Component>>>    where
//        Self: 'a;
//
//    /// Return the last (alive) entry.
//    fn last(&self) -> Option<&Entry<Self::Component>>;
//
//    /// Returns the length of the storage, which is really how many
//    /// indices might be stored (based on the last stored entry),
//    /// *not the number of actual entries*.
//    fn len(&self) -> usize {
//        self.last().map(|e| e.id() + 1).unwrap_or(0)
//    }
//
//    fn is_empty(&self) -> bool {
//        self.len() == 0
//    }
//
//    fn get(&self, id: usize) -> Option<&Self::Component>;
//
//    /// Return an iterator over all entities and indices
//    fn iter(&self) -> Self::Iter<'_>;
//
//    /// Return an iterator over all contiguous entities, regardless
//    /// of whether they reside in the storage.
//    ///
//    /// ## WARNING
//    /// This iterator is unconstrained. It does not terminate.
//    fn maybe(&self) -> MaybeIter<&Entry<Self::Component>, Self::Iter<'_>> {
//        MaybeIter::new(self.iter())
//    }
//
//    fn par_iter(&self) -> Self::ParIter<'_>;
//
//    fn maybe_par_iter(&self) -> MaybeParIter<Self::ParIter<'_>> {
//        MaybeParIter(self.par_iter())
//    }
//}
// impl<T: IsResource + CanReadStorage> CanReadStorage for ReadExpect<T> {
//    type Component = T::Component;
//
//    type Iter<'a> = T::Iter<'a>
//    where
//        Self: 'a;
//
//    type ParIter<'a> = T::ParIter<'a>
//    where
//        Self: 'a;
//
//    fn last(&self) -> Option<&crate::storage::Entry<Self::Component>> {
//        self.deref().last()
//    }
//
//    fn get(&self, id: usize) -> Option<&Self::Component> {
//        self.deref().get(id)
//    }
//
//    fn iter(&self) -> Self::Iter<'_> {
//        self.deref().iter()
//    }
//
//    fn par_iter(&self) -> Self::ParIter<'_> {
//        self.deref().par_iter()
//    }
//}
// impl<T: IsResource + Default + CanReadStorage> CanReadStorage for Read<T> {
//    type Component = T::Component;
//
//    type Iter<'a> = T::Iter<'a>
//    where
//        Self: 'a;
//
//    type ParIter<'a> = T::ParIter<'a>
//    where
//        Self: 'a;
//
//    fn last(&self) -> Option<&crate::storage::Entry<Self::Component>> {
//        self.deref().last()
//    }
//
//    fn get(&self, id: usize) -> Option<&Self::Component> {
//        self.deref().get(id)
//    }
//
//    fn iter(&self) -> Self::Iter<'_> {
//        self.deref().iter()
//    }
//
//    fn par_iter(&self) -> Self::ParIter<'_> {
//        self.deref().par_iter()
//    }
//}
// impl<'a, S: CanReadStorage> CanReadStorage for &'a S {
//    type Component = S::Component;
//
//    type Iter<'b> = S::Iter<'b>
//    where
//        Self: 'b;
//
//    fn last(&self) -> Option<&Entry<Self::Component>> {
//        <S as CanReadStorage>::last(self)
//    }
//
//    fn get(&self, id: usize) -> Option<&Self::Component> {
//        <S as CanReadStorage>::get(self, id)
//    }
//
//    fn iter(&self) -> Self::Iter<'_> {
//        <S as CanReadStorage>::iter(self)
//    }
//
//    type ParIter<'b> = S::ParIter<'b>
//    where
//        Self: 'b;
//
//    fn par_iter(&self) -> Self::ParIter<'_> {
//        <S as CanReadStorage>::par_iter(self)
//    }
//}
// impl<T: IsResource + CanReadStorage> CanReadStorage for WriteExpect<T> {
//    type Component = T::Component;
//
//    type Iter<'a> = T::Iter<'a>
//    where
//        Self: 'a;
//
//    type ParIter<'a> = T::ParIter<'a>
//    where
//        Self: 'a;
//
//    fn get(&self, id: usize) -> Option<&Self::Component> {
//        self.get(id)
//    }
//
//    fn iter(&self) -> Self::Iter<'_> {
//        self.iter()
//    }
//
//    fn par_iter(&self) -> Self::ParIter<'_> {
//        self.par_iter()
//    }
//
//    fn last(&self) -> Option<&crate::storage::Entry<Self::Component>> {
//        self.last()
//    }
//}
//
///// A storage that can be read and written
// pub trait CanWriteStorage: CanReadStorage {
//    type IterMut<'a>: Iterator<Item = &'a mut Entry<Self::Component>>
//    where
//        Self: 'a;
//
//    type ParIterMut<'a>: IndexedParallelIterator<Item = Option<&'a mut
// Entry<Self::Component>>>    where
//        Self: 'a;
//
//    /// Get a mutable reference to the component with the given id.
//    ///
//    /// ## NOTE
//    /// This will cause the component's entry to be marked as changed.
//    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component>;
//
//    fn insert(&mut self, id: usize, component: Self::Component) ->
// Option<Self::Component>;
//
//    fn remove(&mut self, id: usize) -> Option<Self::Component>;
//
//    /// Return an iterator to all entries, mutably.
//    ///
//    /// ## NOTE
//    /// Iterating over the entries mutably will _not_ cause the entries to be
//    /// marked as changed. Only after mutating the underlying component
//    /// through [`DerefMut`] or [`Entry::set_value`], etc, will a change be
//    /// reflected in the entry.
//    fn iter_mut(&mut self) -> Self::IterMut<'_>;
//
//    /// Return a parallel iterator to all entries, mutably.
//    ///
//    /// ## NOTE
//    /// Iterating over the entries mutably will _not_ cause the entries to be
//    /// marked as changed. Only after mutating the underlying component
//    /// through [`DerefMut`] or [`Entry::set_value`], etc, will a change be
//    /// reflected in the entry.
//    fn par_iter_mut(&mut self) -> Self::ParIterMut<'_>;
//
//    /// Return an iterator over all contiguous entities, regardless
//    /// of whether they reside in the storage. Uses mutable components.
//    fn maybe_mut(&mut self) -> MaybeIter<&mut Entry<Self::Component>,
// Self::IterMut<'_>> {        MaybeIter::new(self.iter_mut())
//    }
//
//    fn maybe_par_iter_mut(&mut self) -> MaybeParIter<Self::ParIterMut<'_>> {
//        MaybeParIter(self.par_iter_mut())
//    }
//}
// impl<T: IsResource + CanWriteStorage> CanWriteStorage for WriteExpect<T> {
//    type IterMut<'a> = T::IterMut<'a>
//    where
//        Self: 'a;
//
//    type ParIterMut<'a> = T::ParIterMut<'a>
//    where
//        Self: 'a;
//
//    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
//        self.get_mut(id)
//    }
//
//    fn insert(&mut self, id: usize, component: Self::Component) ->
// Option<Self::Component> {        self.insert(id, component)
//    }
//
//    fn remove(&mut self, id: usize) -> Option<Self::Component> {
//        self.remove(id)
//    }
//
//    fn iter_mut(&mut self) -> Self::IterMut<'_> {
//        self.iter_mut()
//    }
//
//    fn par_iter_mut(&mut self) -> Self::ParIterMut<'_> {
//        self.par_iter_mut()
//    }
//}

///// Storages that can be read from, written to, joined in parallel, created by
///// default and stored as a resource in the world.
// pub trait WorldStorage: CanReadStorage + CanWriteStorage + Default + 'static
// {    /// Create a new storage with a pre-allocated capacity
//    fn new_with_capacity(cap: usize) -> Self;
//
//    /// Perform entity upkeep, defragmentation or any other upkeep on the
//    /// storage. When the storage is added through
//    /// [`WorldStorageExt::with_storage`]
//    /// or [`WorldStorageExt::with_default_storage`], this function will be
//    /// called once per frame, automatically.
//    fn upkeep(&mut self, dead_ids: &[usize]) {
//        for id in dead_ids {
//            let _ = self.remove(*id);
//        }
//    }
//}

// pub trait StoredComponent: Send + Sync + 'static {
//    type StorageType: WorldStorage<Component = Self>;
//}

pub type ReadStore<T> = Read<VecStorage<T>>;
pub type WriteStore<T> = Write<VecStorage<T>>;

// impl<T: IsResource + Default + CanReadStorage> CanReadStorage for Write<T> {
//    type Component = T::Component;
//
//    type Iter<'a> = T::Iter<'a>
//    where
//        Self: 'a;
//
//    type ParIter<'a> = T::ParIter<'a>
//    where
//        Self: 'a;
//
//    fn get(&self, id: usize) -> Option<&Self::Component> {
//        self.get(id)
//    }
//
//    fn iter(&self) -> Self::Iter<'_> {
//        self.iter()
//    }
//
//    fn par_iter(&self) -> Self::ParIter<'_> {
//        self.par_iter()
//    }
//
//    fn last(&self) -> Option<&crate::storage::Entry<Self::Component>> {
//        self.last()
//    }
//}
// impl<T: IsResource + Default + CanWriteStorage> CanWriteStorage for Write<T>
// {    type IterMut<'a> = T::IterMut<'a>
//         where
//             Self: 'a;
//
//    type ParIterMut<'a> = T::ParIterMut<'a>
//         where
//             Self: 'a;
//
//    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
//        self.get_mut(id)
//    }
//
//    fn insert(&mut self, id: usize, component: Self::Component) ->
// Option<Self::Component> {        self.insert(id, component)
//    }
//
//    fn remove(&mut self, id: usize) -> Option<Self::Component> {
//        self.remove(id)
//    }
//
//    fn iter_mut(&mut self) -> Self::IterMut<'_> {
//        self.iter_mut()
//    }
//
//    fn par_iter_mut(&mut self) -> Self::ParIterMut<'_> {
//        self.par_iter_mut()
//    }
//}

#[cfg(test)]
pub mod test {
    use crate::{
        self as apecs,
        storage::{separate::*, Entry, IsEntry},
        system::{ok, ShouldContinue},
        world::*,
        CanFetch,
    };

    #[test]
    pub fn can_track_stores() {
        let mut vs = VecStorage::default();
        vs.insert(0, 0usize);
        vs.insert(1, 1);
        vs.insert(2, 2);

        // let mut rs = RangeStore::default();
        // rs.insert(0, 0);
        // rs.insert(1, 1);
        // rs.insert(2, 2);

        crate::system::increment_current_iteration();

        for v in vs.iter_mut() {
            if *v.value() > 1usize {
                *v.deref_mut() *= 2;
            }
        }

        crate::system::increment_current_iteration();
    }

    pub fn make_abc_vecstorage() -> VecStorage<String> {
        let mut vs = VecStorage::default();
        assert!(vs.insert(0, "abc".to_string()).is_none());
        assert!(vs.insert(1, "def".to_string()).is_none());
        assert!(vs.insert(2, "hij".to_string()).is_none());
        assert!(vs.insert(10, "666".to_string()).is_none());

        vs
    }

    pub fn make_2468_vecstorage() -> VecStorage<i32> {
        let mut vs = VecStorage::default();
        assert!(vs.insert(0, 0).is_none());
        assert!(vs.insert(2, 2).is_none());
        assert!(vs.insert(4, 4).is_none());
        assert!(vs.insert(6, 6).is_none());
        assert!(vs.insert(8, 8).is_none());
        assert!(vs.insert(10, 10).is_none());

        vs
    }

    #[test]
    fn can_tuple_join() {
        let mut entities = Entities::default();
        let mut strings: VecStorage<String> = VecStorage::default();
        let mut numbers: VecStorage<u32> = VecStorage::default();

        let a = entities.create();
        strings.insert(a.id(), "A".to_string());
        numbers.insert(a.id(), 1u32);

        let b = entities.create();
        strings.insert(b.id(), "B".to_string());
        numbers.insert(b.id(), 2u32);

        let c = entities.create();
        strings.insert(c.id(), "C".to_string());

        for (entity, s, n) in (&entities, &mut strings, &numbers).join() {
            s.set_value(format!("{}{}{}", s.value(), entity, n.value()));
        }

        assert_eq!(strings.get(a.id()), Some(&"A01".to_string()));
        assert_eq!(strings.get(b.id()), Some(&"B12".to_string()));
        assert_eq!(strings.get(c.id()), Some(&"C".to_string()));
    }

    #[test]
    fn can_par_join_system() {
        #[derive(CanFetch)]
        struct Data {
            a: Write<VecStorage<f32>>,
            b: Write<VecStorage<&'static str>>,
        }

        fn system<const IS_PAR: bool>(mut data: Data) -> anyhow::Result<ShouldContinue> {
            if IS_PAR {
                (&mut data.a, &mut data.b)
                    .par_join()
                    .for_each(|(number, _string)| {
                        *number.deref_mut() *= -1.0;
                    });
            } else {
                for (number, _string) in (&mut data.a, &mut data.b).join() {
                    number.set_value(number.value() * -1.0);
                }
            }

            ok()
        }

        fn run<A, B, const IS_PAR: bool>() -> anyhow::Result<()> {
            let desc = format!(
                "{}-{}",
                std::any::type_name::<A>(),
                if IS_PAR { "parallel" } else { "seq" }
            );

            let mut world = World::default();
            world
                .with_resource(VecStorage::<f32>::new_with_capacity(1000))?
                .with_resource(VecStorage::<&'static str>::new_with_capacity(1000))?
                .with_system("ab", system::<IS_PAR>)?;

            // build the world
            {
                let (mut entities, mut a, mut b): (
                    Write<Entities>,
                    Write<VecStorage<f32>>,
                    Write<VecStorage<&'static str>>,
                ) = world.fetch()?;
                for n in 0..1000 {
                    let e = entities.create();
                    a.insert(e.id(), 1.0);
                    if n % 2 == 0 {
                        b.insert(e.id(), "1.0");
                    }
                }
                // affirm the state of the world
                let sum: f32 = if IS_PAR {
                    a.par_iter()
                        .into_par_iter()
                        .flatten_iter()
                        .map(|e| e.value)
                        .sum()
                } else {
                    a.iter().map(|e| e.value).sum()
                };

                assert_eq!(sum, 1000.0, "{} sum fail", desc);
            }

            // run the world
            world.tick_sync()?;

            // assert the state of the world
            let a: Read<VecStorage<f32>> = world.fetch()?;
            let sum: f32 = a.iter().map(|e| e.value).sum();
            assert_eq!(sum, 0.0, "{} join failed", desc);

            Ok(())
        }

        run::<VecStorage<f32>, VecStorage<&'static str>, false>().unwrap();
        // run::<RangeStore<f32>, RangeStore<&'static str>, false>().unwrap();

        run::<VecStorage<f32>, VecStorage<&'static str>, true>().unwrap();
        // run::<RangeStore<f32>, RangeStore<&'static str>, true>().unwrap();
    }

    #[test]
    fn can_join_not() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();
        let joined = (&vs_abc, !&vs_246)
            .join()
            .map(|(s, t)| {
                assert!(s.id() == t.id());
                (s.id(), s.value().as_str(), t.value().clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(joined, vec![(1, "def", ()),]);

        let joined = (&vs_abc, !&vs_246)
            .par_join()
            .map(|(e, n)| (e.as_str(), n))
            .collect::<Vec<_>>();
        assert_eq!(joined, vec![("def", ()),]);
    }

    #[test]
    fn can_join_maybe() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        fn unwrap<'a, 'b: 'a>(
            iter: &'a mut impl Iterator<Item = (&'b Entry<String>, Maybe<&'b Entry<i32>>)>,
        ) -> (usize, String, Option<i32>) {
            let (s, Maybe { key, inner }) = iter.next().unwrap();
            assert!(s.id() == key);
            (
                s.id(),
                s.value().clone(),
                inner.as_ref().map(|e| *e.value()),
            )
        }

        let mut joined = (&vs_abc, vs_246.maybe()).join();
        assert_eq!(unwrap(&mut joined), (0, "abc".to_string(), Some(0)));
        assert_eq!(unwrap(&mut joined), (1, "def".to_string(), None));
        assert_eq!(unwrap(&mut joined), (2, "hij".to_string(), Some(2)));
        assert_eq!(unwrap(&mut joined), (10, "666".to_string(), Some(10)));
        assert_eq!(joined.next(), None);

        // let joined = (&vs_abc, vs_246.maybe_par_iter())
        //    .par_join()
        //    .map(|(es, ns)| (es.as_str(), ns.as_ref().map(|e| e.value)))
        //    .collect::<Vec<_>>();
        // assert_eq!(
        //    joined,
        //    vec![
        //        ("abc", Some(0)),
        //        ("def", None),
        //        ("hij", Some(2)),
        //        ("666", Some(10))
        //    ]
        //);

        // let mut range = RangeStore::<usize>::default();
        // range.insert(0, 0);
        // let joined = (&range,).par_join().map(|e|
        // e.value).collect::<Vec<_>>(); assert_eq!(joined, vec![0]);
        // let joined = (&mut range,)
        //    .par_join()
        //    .map(|e| e.value)
        //    .collect::<Vec<_>>();
        // assert_eq!(joined, vec![0]);
    }

    #[test]
    fn can_join_a_b() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        let mut iter = (&vs_abc, &vs_246)
            .join()
            .map(|(s, i)| (s.id(), s.as_str(), *i.value()));
        assert_eq!(iter.next(), Some((0, "abc", 0)));
        assert_eq!(iter.next(), Some((2, "hij", 2)));
        assert_eq!(iter.next(), Some((10, "666", 10)));
    }

    #[test]
    fn entities_create() {
        let mut entities = Entities::default();
        let a = entities.create();
        let b = entities.create();
        let c = entities.create();

        assert_eq!(a.id(), 0);
        assert_eq!(b.id(), 1);
        assert_eq!(c.id(), 2);
    }

    #[test]
    fn entity_create_storage_insert() {
        struct A(f32);

        let mut entities = Entities::default();
        let mut a_storage: VecStorage<A> = VecStorage::default();
        let _ = (0..10000)
            .map(|_| {
                let e = entities.create();
                let _ = a_storage.insert(e.id(), A(0.0));
                e
            })
            .collect::<Vec<_>>();
    }

    #[test]
    fn choose_system_iteration_type() {
        let max = u64::MAX as f32;
        let years: f32 = max / (1000.0 * 60.0 * 60.0 * 60.0 * 24.0 * 365.0);
        assert!(1.0 <= years, "{} years", years);
    }

    //#[test]
    // fn can_derive_storedcomponent() {
    //    #[derive(Debug, StoredComponent_Vec)]
    //    struct MyComponent(f32);

    //    let _store: <MyComponent as StoredComponent>::StorageType =
    // Default::default();

    //    #[derive(Debug, StoredComponent_Range)]
    //    struct OtherComponent(f32);

    //    let _store: <OtherComponent as StoredComponent>::StorageType =
    // Default::default();
    //}
}
