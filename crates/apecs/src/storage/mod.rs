//! Entity component storage traits.
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use std::{
    ops::{Deref, DerefMut},
    sync::atomic::AtomicU64,
};

mod range_map;
pub use range_map::*;
mod tracking;
pub use tracking::*;
mod vec;
pub use vec::*;

use crate::{Read, Write, ReadExpect, WriteExpect};

pub mod bitset {
    pub use hibitset::*;
}

static SYSTEM_ITERATION: AtomicU64 = AtomicU64::new(0);

pub fn clear_iteration() {
    SYSTEM_ITERATION.store(0, std::sync::atomic::Ordering::SeqCst)
}

pub fn current_iteration() -> u64 {
    SYSTEM_ITERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// Increment the system iteration counter, returning the previous value.
pub fn increment_current_iteration() -> u64 {
    SYSTEM_ITERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

pub trait IsEntry {
    type Component;

    fn id(&self) -> usize;
}

impl IsEntry for usize {
    type Component = usize;

    fn id(&self) -> usize {
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
    type Component = T;

    fn id(&self) -> usize {
        self.id()
    }
}

impl<'a, T> IsEntry for &'a Entry<T> {
    type Component = T;

    fn id(&self) -> usize {
        self.key
    }
}

impl<'a, T> IsEntry for &'a mut Entry<T> {
    type Component = T;

    fn id(&self) -> usize {
        self.key
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
            changed: 0,
            added: true,
        }
    }

    fn mark_changed(&mut self) {
        self.changed = current_iteration();
        self.added = false;
    }

    pub fn has_changed_since(&self, iteration: u64) -> bool {
        self.changed > iteration
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

pub struct Without<T>(pub T);

impl<'a, T> IntoParallelIterator for Without<&'a T>
where
    T: CanReadStorage,
    T::ParIter<'a>: Send + Sync,
{
    type Iter = rayon::iter::Map<T::ParIter<'a>, fn(Option<&'a Entry<T::Component>>) -> Option<()>>;

    type Item = Option<()>;

    fn into_par_iter(self) -> Self::Iter {
        self.0
            .par_iter()
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

#[derive(Clone, Debug, PartialEq)]
pub struct Maybe<T> {
    pub key: usize,
    pub inner: Option<T>,
}

impl<T> IsEntry for Maybe<T> {
    type Component = T;

    fn id(&self) -> usize {
        self.key
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

/// A storage that can only be read from
pub trait CanReadStorage: Send + Sync {
    type Component: Send + Sync;

    type Iter<'a>: Iterator<Item = &'a Entry<Self::Component>>
    where
        Self: 'a;

    type ParIter<'a>: IndexedParallelIterator<Item = Option<&'a Entry<Self::Component>>>
    where
        Self: 'a;

    /// Return the last (alive) entry.
    fn last(&self) -> Option<&Entry<Self::Component>>;

    /// Returns the length of the storage, which is really how many
    /// indices might be stored (based on the last stored entry),
    /// *not the number of actual entries*.
    fn len(&self) -> usize {
        self.last().map(|e| e.id() + 1).unwrap_or(0)
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, id: usize) -> Option<&Self::Component>;

    /// Return an iterator over all entities and indices
    fn iter(&self) -> Self::Iter<'_>;

    /// Return an iterator over all contiguous entities, regardless
    /// of whether they reside in the storage.
    ///
    /// ## WARNING
    /// This iterator is unconstrained. It does not terminate.
    fn maybe(&self) -> MaybeIter<&Entry<Self::Component>, Self::Iter<'_>> {
        MaybeIter::new(self.iter())
    }

    fn par_iter(&self) -> Self::ParIter<'_>;

    fn maybe_par_iter(&self) -> MaybeParIter<Self::ParIter<'_>> {
        MaybeParIter(self.par_iter())
    }
}

impl<'a, S: CanReadStorage> CanReadStorage for &'a S {
    type Component = S::Component;

    type Iter<'b> = S::Iter<'b>
    where
        Self: 'b;

    fn last(&self) -> Option<&Entry<Self::Component>> {
        <S as CanReadStorage>::last(self)
    }

    fn get(&self, id: usize) -> Option<&Self::Component> {
        <S as CanReadStorage>::get(self, id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        <S as CanReadStorage>::iter(self)
    }

    type ParIter<'b> = S::ParIter<'b>
    where
        Self: 'b;

    fn par_iter(&self) -> Self::ParIter<'_> {
        <S as CanReadStorage>::par_iter(self)
    }

}

/// A storage that can be read and written
pub trait CanWriteStorage: CanReadStorage {
    type IterMut<'a>: Iterator<Item = &'a mut Entry<Self::Component>>
    where
        Self: 'a;

    type ParIterMut<'a>: IndexedParallelIterator<Item = Option<&'a mut Entry<Self::Component>>>
    where
        Self: 'a;

    /// Get a mutable reference to the component with the given id.
    ///
    /// ## NOTE
    /// This will cause the component's entry to be marked as changed.
    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component>;

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component>;

    fn remove(&mut self, id: usize) -> Option<Self::Component>;

    /// Return an iterator to all entries, mutably.
    ///
    /// ## NOTE
    /// Iterating over the entries mutably will _not_ cause the entries to be marked as
    /// changed. Only after mutating the underlying component through [`DerefMut`] or
    /// [`Entry::set_value`], etc, will a change be reflected in the entry.
    fn iter_mut(&mut self) -> Self::IterMut<'_>;

    /// Return a parallel iterator to all entries, mutably.
    ///
    /// ## NOTE
    /// Iterating over the entries mutably will _not_ cause the entries to be marked as
    /// changed. Only after mutating the underlying component through [`DerefMut`] or
    /// [`Entry::set_value`], etc, will a change be reflected in the entry.
    fn par_iter_mut(&mut self) -> Self::ParIterMut<'_>;

    /// Return an iterator over all contiguous entities, regardless
    /// of whether they reside in the storage. Uses mutable components.
    fn maybe_mut(&mut self) -> MaybeIter<&mut Entry<Self::Component>, Self::IterMut<'_>> {
        MaybeIter::new(self.iter_mut())
    }

    fn maybe_par_iter_mut(&mut self) -> MaybeParIter<Self::ParIterMut<'_>> {
        MaybeParIter(self.par_iter_mut())
    }
}

impl<'a, S: CanReadStorage<Component = T> + 'static, T: Send + Sync + 'a> IntoParallelIterator
    for &'a ReadExpect<S>
{
    type Iter = S::ParIter<'a>;

    type Item = Option<&'a Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter().into_par_iter()
    }
}

impl<'a, S: Default + CanReadStorage<Component = T> + 'static, T: Send + Sync + 'a> IntoParallelIterator
    for &'a Read<S>
{
    type Iter = S::ParIter<'a>;

    type Item = Option<&'a Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter().into_par_iter()
    }
}

impl<'a, S: CanWriteStorage<Component = T> + 'static, T: Send + 'a> IntoParallelIterator
    for &'a mut WriteExpect<S>
{
    type Iter = S::ParIterMut<'a>;

    type Item = Option<&'a mut Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter_mut().into_par_iter()
    }
}

impl<'a, S: CanWriteStorage<Component = T> + 'static, T: Send + Sync + 'a> IntoParallelIterator
    for &'a WriteExpect<S>
{
    type Iter = S::ParIter<'a>;

    type Item = Option<&'a Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter().into_par_iter()
    }
}

impl<'a, S: Default + CanWriteStorage<Component = T> + 'static, T: Send + 'a> IntoParallelIterator
    for &'a mut Write<S>
{
    type Iter = S::ParIterMut<'a>;

    type Item = Option<&'a mut Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter_mut().into_par_iter()
    }
}

impl<'a, S: Default + CanWriteStorage<Component = T> + 'static, T: Send + Sync + 'a> IntoParallelIterator
    for &'a Write<S>
{
    type Iter = S::ParIter<'a>;

    type Item = Option<&'a Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter().into_par_iter()
    }
}

/// Storages that can be read from, written to, joined in parallel, created by default
/// and stored as a resource in the world.
pub trait WorldStorage:
    CanReadStorage + CanWriteStorage + Default + 'static
{
    /// Create a new storage with a pre-allocated capacity
    fn new_with_capacity(cap: usize) -> Self;

    /// Perform entity upkeep, defragmentation or any other upkeep on the storage.
    /// When the storage is added through [`WorldStorageExt::with_storage`]
    /// or [`WorldStorageExt::with_default_storage`], this function will be
    /// called once per frame, automatically.
    fn upkeep(&mut self, dead_ids: &[usize]) {
        for id in dead_ids {
            let _ = self.remove(*id);
        }
    }
}

pub trait StoredComponent: Send + Sync + 'static {
    type StorageType: WorldStorage<Component = Self>;
}

pub type ReadStore<T> = Read<<T as StoredComponent>::StorageType>;
pub type WriteStore<T> = Write<<T as StoredComponent>::StorageType>;

#[cfg(test)]
pub mod test {
    use crate::{
        self as apecs,
        entities::*,
        join::*,
        storage::*,
        system::{ok, ShouldContinue},
        world::World,
        CanFetch,
    };

    #[test]
    pub fn can_track_stores() {
        let mut vs = VecStorage::default();
        vs.insert(0, 0usize);
        vs.insert(1, 1);
        vs.insert(2, 2);

        let mut rs = RangeStore::default();
        rs.insert(0, 0);
        rs.insert(1, 1);
        rs.insert(2, 2);

        increment_current_iteration();

        for v in (&mut vs,).join() {
            if *v.value() > 1usize {
                *v.deref_mut() *= 2;
            }
        }

        increment_current_iteration();


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
    fn can_thruple_join() {
        let mut entities = Entities::new();
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
        struct Data<A, B>
        where
            A: WorldStorage<Component = f32>,
            B: WorldStorage<Component = &'static str>,
        {
            a: Write<A>,
            b: Write<B>,
        }

        fn system<A, B, const IS_PAR: bool>(mut data: Data<A, B>) -> anyhow::Result<ShouldContinue>
        where
            A: WorldStorage<Component = f32>,
            B: WorldStorage<Component = &'static str>,
        {
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

        fn run<A, B, const IS_PAR: bool>() -> anyhow::Result<()>
        where
            A: WorldStorage<Component = f32>,
            B: WorldStorage<Component = &'static str>,
        {
            let desc = format!(
                "{}-{}",
                std::any::type_name::<A>(),
                if IS_PAR { "parallel" } else { "seq" }
            );

            let mut world = World::default();
            world
                .with_default_resource::<Entities>()?
                .with_resource(A::new_with_capacity(1000))?
                .with_resource(B::new_with_capacity(1000))?
                .with_system("ab", system::<A, B, IS_PAR>)?;

            // build the world
            {
                let (mut entities, mut a, mut b): (Write<Entities>, Write<A>, Write<B>) =
                    world.fetch()?;
                for n in 0..1000 {
                    let e = entities.create();
                    a.insert(e.id(), 1.0);
                    if n % 2 == 0 {
                        b.insert(e.id(), "1.0");
                    }
                }
                // affirm the state of the world
                let sum: f32 = if IS_PAR {
                    a.par_iter().into_par_iter().flatten_iter().map(|e| e.value).sum()
                } else {
                    a.iter().map(|e| e.value).sum()
                };

                assert_eq!(sum, 1000.0, "{} sum fail", desc);
            }

            // run the world
            world.tick_sync()?;

            // assert the state of the world
            let a: Read<A> = world.fetch()?;
            let sum: f32 = a.iter().map(|e| e.value).sum();
            assert_eq!(sum, 0.0, "{} join failed", desc);

            Ok(())
        }

        run::<VecStorage<f32>, VecStorage<&'static str>, false>().unwrap();
        run::<RangeStore<f32>, RangeStore<&'static str>, false>().unwrap();

        run::<VecStorage<f32>, VecStorage<&'static str>, true>().unwrap();
        run::<RangeStore<f32>, RangeStore<&'static str>, true>().unwrap();
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

        let joined = (&vs_abc, !&vs_246).par_join().map(|(e, n)| (e.as_str(), n)).collect::<Vec<_>>();
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

        let joined = (&vs_abc, vs_246.maybe_par_iter())
            .par_join()
            .map(|(es, ns)| (es.as_str(), ns.as_ref().map(|e| e.value)))
            .collect::<Vec<_>>();
        assert_eq!(
            joined,
            vec![
                ("abc", Some(0)),
                ("def", None),
                ("hij", Some(2)),
                ("666", Some(10))
            ]
        );

        let mut range = RangeStore::<usize>::default();
        range.insert(0, 0);
        let joined = (&range,).par_join().map(|e| e.value).collect::<Vec<_>>();
        assert_eq!(joined, vec![0]);
        let joined = (&mut range,).par_join().map(|e| e.value).collect::<Vec<_>>();
        assert_eq!(joined, vec![0]);
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
}
