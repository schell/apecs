//! Entity component storage traits.
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use std::sync::{Arc, Mutex};

mod vec;
pub use vec::*;

mod btree;
pub use btree::*;

pub mod tracking;

use crate::{Read, Write};

pub mod bitset {
    pub use hibitset::*;
}

pub trait StorageComponent {
    type Component;

    fn split(self) -> (usize, Self::Component);

    fn id(&self) -> usize;

    fn value(self) -> Self::Component;
}

impl<A> StorageComponent for (usize, A) {
    type Component = A;

    fn split(self) -> (usize, Self::Component) {
        self
    }

    fn id(&self) -> usize {
        self.0
    }

    fn value(self) -> Self::Component {
        self.1
    }
}

impl StorageComponent for usize {
    type Component = usize;

    fn split(self) -> (usize, Self::Component) {
        (self, self)
    }

    fn id(&self) -> usize {
        *self
    }

    fn value(self) -> Self::Component {
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry<T> {
    pub(crate) key: usize,
    pub value: T,
}

impl<T> StorageComponent for Entry<T> {
    type Component = T;

    fn split(self) -> (usize, Self::Component) {
        (self.key, self.value)
    }

    fn id(&self) -> usize {
        self.key()
    }

    fn value(self) -> Self::Component {
        self.value
    }
}

impl<T> Entry<T> {
    pub fn key(&self) -> usize {
        self.key
    }

    pub fn as_ref(&self) -> Entry<&T> {
        Entry {
            key: self.key,
            value: &self.value,
        }
    }

    pub fn as_mut(&mut self) -> Entry<&mut T> {
        Entry {
            key: self.key,
            value: &mut self.value,
        }
    }

    pub fn map<X>(self, f: impl FnOnce(T) -> X) -> Entry<X> {
        Entry {
            key: self.key,
            value: f(self.value),
        }
    }
}

pub struct Without<T>(pub T);

impl<'a, T> IntoParallelIterator for Without<&'a T>
where
    T: ParallelStorage,
    T::ParIter<'a>: Send + Sync,
{
    type Iter = rayon::iter::Map<T::ParIter<'a>, fn(Option<&'a T::Component>) -> Option<()>>;

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

impl<T> WithoutIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
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

impl<T: Iterator> Iterator for WithoutIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
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
        };
        self.id += 1;
        Some(entry)
    }
}

pub struct MaybeIter<T: Iterator>
where
    T::Item: StorageComponent,
{
    iter: T,
    id: usize,
    next_id: usize,
    next_value: Option<<T::Item as StorageComponent>::Component>,
}

fn maybe_next<T: Iterator>(
    iter: &mut T,
) -> (usize, Option<<T::Item as StorageComponent>::Component>)
where
    T::Item: StorageComponent,
{
    iter.next()
        .map(|mn| {
            let (id, value) = mn.split();
            (id, Some(value))
        })
        .unwrap_or_else(|| (usize::MAX, None))
}

impl<T> MaybeIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
{
    pub(crate) fn new(mut iter: T) -> Self {
        let (next_id, next_value) = maybe_next(&mut iter);
        MaybeIter {
            iter,
            id: 0,
            next_id,
            next_value,
        }
    }
}

impl<T: Iterator> Iterator for MaybeIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
{
    type Item = Entry<Option<<T::Item as StorageComponent>::Component>>;

    fn next(&mut self) -> Option<Self::Item> {
        let value = if self.id == self.next_id {
            let (next_id, next_value) = maybe_next(&mut self.iter);
            self.next_id = next_id;
            std::mem::replace(&mut self.next_value, next_value)
        } else {
            None
        };
        let entry = Entry {
            key: self.id,
            value,
        };
        self.id += 1;
        Some(entry)
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
pub trait CanReadStorage {
    type Component;

    type Iter<'a>: Iterator<Item = Entry<&'a Self::Component>>
    where
        Self: 'a;

    /// Return the last (alive) entry.
    fn last(&self) -> Option<Entry<&Self::Component>>;

    /// Returns the length of the storage, which is really how many
    /// indices might be stored (based on the last stored entry),
    /// *not the number of actual entries*.
    fn len(&self) -> usize {
        self.last().map(|e| e.key() + 1).unwrap_or(0)
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, id: usize) -> Option<&Self::Component>;

    /// Return an iterator over all entities and indices
    fn iter(&self) -> Self::Iter<'_>;

    /// Return an iterator over all contiguous entities, regardless
    /// of whether they reside in the storage.
    fn maybe(&self) -> MaybeIter<Self::Iter<'_>> {
        MaybeIter::new(self.iter())
    }
}

impl<'a, S: CanReadStorage> CanReadStorage for &'a S {
    type Component = S::Component;

    type Iter<'b> = S::Iter<'b>
    where
        Self: 'b;

    fn last(&self) -> Option<Entry<&Self::Component>> {
        <S as CanReadStorage>::last(self)
    }

    fn get(&self, id: usize) -> Option<&Self::Component> {
        <S as CanReadStorage>::get(self, id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        <S as CanReadStorage>::iter(self)
    }
}

/// A storage that can be read and written
pub trait CanWriteStorage: CanReadStorage {
    type IterMut<'a>: Iterator<Item = Entry<&'a mut Self::Component>>
    where
        Self: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component>;

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component>;

    fn remove(&mut self, id: usize) -> Option<Self::Component>;

    /// Return an iterator over entities, with the mutable components and their indices.
    fn iter_mut(&mut self) -> Self::IterMut<'_>;

    /// Return an iterator over all contiguous entities, regardless
    /// of whether they reside in the storage. Uses mutable components.
    fn maybe_mut(&mut self) -> MaybeIter<Self::IterMut<'_>> {
        MaybeIter::new(self.iter_mut())
    }
}

/// A helper for writing `IntoParallelIterator` implementations for references to storages.
pub struct StorageIter<'a, S>(&'a S);

impl<'a, S> IntoParallelIterator for StorageIter<'a, S>
where
    S: CanReadStorage + Send + Sync,
    S::Component: Send + Sync,
{
    type Iter = rayon::iter::MapWith<
        rayon::range::Iter<usize>,
        &'a S,
        fn(&mut &'a S, usize) -> Option<&'a S::Component>,
    >;

    type Item = Option<&'a S::Component>;

    fn into_par_iter(self) -> Self::Iter {
        fn get<'a, S: CanReadStorage>(s: &mut &'a S, n: usize) -> Option<&'a S::Component> {
            s.get(n)
        }

        let range = 0..self.0.len();
        let iter: _ = range.into_par_iter().map_with(
            self.0,
            get as fn(&mut &'a S, usize) -> Option<&'a S::Component>,
        );
        iter
    }
}

impl<'a, T> StorageIter<'a, T>
where
    T: CanReadStorage,
{
    pub fn new(s: &'a T) -> Self {
        StorageIter(s)
    }

    pub fn get(&self, id: usize) -> Option<&'a T::Component> {
        self.0.get(id)
    }
}

/// A helper for writing `IntoParallelIterator` implementations for references to storages.
pub struct StorageIterMut<'a, S>(&'a mut S);

pub type StorageIterGetFn<'a, S> =
    fn(&mut Arc<Mutex<&'a mut S>>, usize) -> Option<&'a mut <S as CanReadStorage>::Component>;

impl<'a, S> IntoParallelIterator for StorageIterMut<'a, S>
where
    S: CanWriteStorage + Send + Sync,
    S::Component: Send + Sync,
{
    type Iter = rayon::iter::MapWith<
        rayon::range::Iter<usize>,
        Arc<Mutex<&'a mut S>>,
        StorageIterGetFn<'a, S>,
    >;

    type Item = Option<&'a mut S::Component>;

    fn into_par_iter(self) -> Self::Iter {
        fn get_mut<'a, S: CanWriteStorage>(
            s: &mut Arc<Mutex<&'a mut S>>,
            n: usize,
        ) -> Option<&'a mut S::Component> {
            s.lock()
                .unwrap()
                .get_mut(n)
                .map(|t: &mut S::Component| -> &'a mut S::Component {
                    //modified.try_send(n).unwrap();
                    // we know that the index is monotonically increasing, so we will
                    // never give out a reference to the same item twice, so this is
                    // tolerably unsafe
                    unsafe { &mut *(t as *mut S::Component) }
                })
        }

        let range = 0..self.0.len();
        let a: Arc<Mutex<&'a mut S>> = Arc::new(Mutex::new(self.0));
        let iter: _ = range
            .into_par_iter()
            .map_with(a, get_mut as StorageIterGetFn<'a, S>);
        iter
    }
}

pub trait ParallelStorage: CanReadStorage + CanWriteStorage + Send + Sync {
    /// The indexed parallel iterator
    type ParIter<'a>: IndexedParallelIterator<Item = Option<&'a Self::Component>>
    where
        Self: 'a;
    /// A helper type that can be turned into `Self::ParIter<'a>` above.
    type IntoParIter<'a>: IntoParallelIterator<Iter = Self::ParIter<'a>>
    where
        Self: 'a;

    /// The mutable indexed parallel iterator
    type ParIterMut<'a>: IndexedParallelIterator<Item = Option<&'a mut Self::Component>>
    where
        Self: 'a;
    /// A helper type that can be turned into `Self::ParIterMut<'a>` above.
    type IntoParIterMut<'a>: IntoParallelIterator<Iter = Self::ParIterMut<'a>>
    where
        Self: 'a;

    fn par_iter(&self) -> Self::IntoParIter<'_>;

    fn par_iter_mut(&mut self) -> Self::IntoParIterMut<'_>;

    fn maybe_par_iter(&self) -> MaybeParIter<Self::IntoParIter<'_>> {
        MaybeParIter(self.par_iter())
    }

    fn maybe_par_iter_mut(&mut self) -> MaybeParIter<Self::IntoParIterMut<'_>> {
        MaybeParIter(self.par_iter_mut())
    }
}

impl<'a, S: ParallelStorage<Component = T> + 'static, T: Send + Sync + 'a> IntoParallelIterator
    for &'a Read<S>
{
    type Iter = <S as ParallelStorage>::ParIter<'a>;

    type Item = Option<&'a T>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter().into_par_iter()
    }
}

impl<'a, S: ParallelStorage<Component = T> + 'static, T: Send + 'a> IntoParallelIterator
    for &'a mut Write<S>
{
    type Iter = <S as ParallelStorage>::ParIterMut<'a>;

    type Item = Option<&'a mut T>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter_mut().into_par_iter()
    }
}

impl<'a, S: ParallelStorage<Component = T> + 'static, T: Send + Sync + 'a> IntoParallelIterator
    for &'a Write<S>
{
    type Iter = <S as ParallelStorage>::ParIter<'a>;

    type Item = Option<&'a T>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter().into_par_iter()
    }
}

/// Storages that can be read from, written to, joined in parallel, created by default
/// and stored as a resource in the world.
pub trait WorldStorage:
    CanReadStorage + CanWriteStorage + ParallelStorage + Default + 'static
{
    /// Create a new storage with a pre-allocated capacity
    fn new_with_capacity(cap: usize) -> Self;
}

#[cfg(test)]
pub mod test {
    use crate::{self as apecs, entities::*, join::*, storage::*, world::World, CanFetch};

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

        for (_, entity, s, n) in (&entities, &mut strings, &numbers).join() {
            *s = format!("{}{}{}", s, entity.id(), n);
        }

        assert_eq!(strings.get(a.id()), Some(&"A01".to_string()));
        assert_eq!(strings.get(b.id()), Some(&"B12".to_string()));
        assert_eq!(strings.get(c.id()), Some(&"C".to_string()));
    }

    #[test]
    fn can_par_join() {
        #[derive(CanFetch)]
        struct Data<A, B>
        where
            A: WorldStorage<Component = f32>,
            B: WorldStorage<Component = &'static str>,
        {
            a: Write<A>,
            b: Write<B>,
        }

        fn system<A, B, const IS_PAR: bool>(mut data: Data<A, B>) -> anyhow::Result<()>
        where
            A: WorldStorage<Component = f32>,
            B: WorldStorage<Component = &'static str>,
        {
            if IS_PAR {
                (&mut data.a, &mut data.b)
                    .par_join()
                    .for_each(|(number, _string)| {
                        *number *= -1.0;
                    });
            } else {
                for (_, number, _string) in (&mut data.a, &mut data.b).join() {
                    *number *= -1.0;
                }
            }

            Ok(())
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
                .with_system("ab", system::<A, B, IS_PAR>);

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
                    a.par_iter().into_par_iter().flatten_iter().sum()
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
        //run::<SparseStorage<f32>, SparseStorage<&'static str>, false>().unwrap();
        run::<BTreeStorage<f32>, BTreeStorage<&'static str>, false>().unwrap();

        run::<VecStorage<f32>, VecStorage<&'static str>, true>().unwrap();
        //run::<SparseStorage<f32>, SparseStorage<&'static str>, true>().unwrap();
        run::<BTreeStorage<f32>, BTreeStorage<&'static str>, true>().unwrap();
    }

    #[test]
    fn can_join_not() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();
        let joined = (&vs_abc, !&vs_246).join().collect::<Vec<_>>();
        assert_eq!(joined, vec![(1, &"def".to_string(), ()),]);

        let joined = (&vs_abc, !&vs_246).par_join().collect::<Vec<_>>();
        assert_eq!(joined, vec![(&"def".to_string(), ()),]);
    }

    #[test]
    fn can_join_maybe() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        let mut joined = (&vs_abc, vs_246.maybe()).join();
        assert_eq!(joined.next().unwrap(), (0, &"abc".to_string(), Some(&0)));
        assert_eq!(joined.next().unwrap(), (1, &"def".to_string(), None));
        assert_eq!(joined.next().unwrap(), (2, &"hij".to_string(), Some(&2)));
        assert_eq!(joined.next().unwrap(), (10, &"666".to_string(), Some(&10)));
        assert_eq!(joined.next(), None);

        let joined = (&vs_abc, vs_246.maybe_par_iter())
            .par_join()
            .collect::<Vec<_>>();
        assert_eq!(
            joined,
            vec![
                (&"abc".to_string(), Some(&0)),
                (&"def".to_string(), None),
                (&"hij".to_string(), Some(&2)),
                (&"666".to_string(), Some(&10))
            ]
        );

        let mut btree = BTreeStorage::<usize>::default();
        btree.insert(0, 0);
        let joined = (&btree,).par_join().collect::<Vec<_>>();
        assert_eq!(joined, vec![&0]);
        let joined = (&mut btree,).par_join().collect::<Vec<_>>();
        assert_eq!(joined, vec![&0]);
    }

    #[test]
    fn can_join_a_b() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        let mut iter = (&vs_abc, &vs_246).join();
        assert_eq!(iter.next(), Some((0, &"abc".to_string(), &0)));
        assert_eq!(iter.next(), Some((2, &"hij".to_string(), &2)));
        assert_eq!(iter.next(), Some((10, &"666".to_string(), &10)));
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
}
