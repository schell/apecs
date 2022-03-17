//! Entity component storage trait and implementations.
use std::{
    collections::VecDeque,
    iter::{Cycle, Once},
    ops::Deref,
};

use hibitset::{BitSet, BitSetAll, BitSetAnd, BitSetLike, BitSetNot};
pub use itertools::{multizip, Zip};

use crate::{IsResource, Read, Write};

pub struct StorageIter<'a, T> {
    iter: Box<dyn Iterator<Item = (u32, Option<&'a T>)> + 'a>,
}

impl<'a, T> Iterator for StorageIter<'a, T> {
    type Item = (u32, Option<&'a T>);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

pub struct StorageIterMut<'a, T> {
    iter: Box<dyn Iterator<Item = (u32, Option<&'a mut T>)> + 'a>,
}

impl<'a, T> Iterator for StorageIterMut<'a, T> {
    type Item = (u32, Option<&'a mut T>);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

pub struct JoinIter<'a, T> {
    iter: Box<dyn Iterator<Item = T> + 'a>,
}

impl<'a, T> Iterator for JoinIter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl<'a, T> JoinIter<'a, T> {
    pub fn new(iter: impl Iterator<Item = T> + 'a) -> Self {
        JoinIter {
            iter: Box::new(iter),
        }
    }
}

pub trait Storage: Sized {
    type Component;

    fn new() -> Self;

    fn mask(&self) -> &BitSet;

    fn get(&self, index: &u32) -> Option<&Self::Component>;

    fn get_mut(&mut self, index: &u32) -> Option<&mut Self::Component>;

    fn insert(&mut self, index: u32, component: Self::Component) -> Option<Self::Component>;

    fn remove(&mut self, index: &u32) -> Option<Self::Component>;

    fn iter(&self) -> StorageIter<'_, Self::Component>;

    /// Return an iterator over all entities, with the components whose indices are **not**
    /// contained in the mask set to `None`.
    fn masked_iter<M: StaticMask>(&self, mask: M) -> JoinIter<'_, Option<&Self::Component>>;

    fn iter_mut(&mut self) -> StorageIterMut<'_, Self::Component>;

    /// Return an iterator over all entities, with the mutable components whose indices are
    /// **not** contained in the mask set to `None`.
    fn masked_iter_mut<M: StaticMask>(
        &mut self,
        mask: M,
    ) -> JoinIter<'_, Option<&mut Self::Component>>;
}

pub struct VecStorage<T> {
    mask: BitSet,
    inner: Vec<Option<T>>,
}

impl<T> Default for VecStorage<T> {
    fn default() -> Self {
        Self {
            mask: Default::default(),
            inner: Default::default(),
        }
    }
}

impl<'a, T> std::ops::Not for &'a VecStorage<T> {
    type Output = Without<Self>;

    fn not(self) -> Self::Output {
        Without(self)
    }
}

// TODO: Figure out how to ditch the Clone constraint
impl<T: Clone> Storage for VecStorage<T> {
    type Component = T;

    fn new() -> Self {
        VecStorage {
            mask: BitSet::new(),
            inner: vec![],
        }
    }

    fn mask(&self) -> &BitSet {
        &self.mask
    }

    fn get(&self, index: &u32) -> Option<&Self::Component> {
        self.inner
            .get(*index as usize)
            .map(|p| p.as_ref())
            .flatten()
    }

    fn get_mut(&mut self, index: &u32) -> Option<&mut Self::Component> {
        self.inner
            .get_mut(*index as usize)
            .map(|p| p.as_mut())
            .flatten()
    }

    fn insert(&mut self, index: u32, component: Self::Component) -> Option<Self::Component> {
        let id = index as usize;
        let prev = if self.mask.contains(index) {
            self.inner.remove(id)
        } else {
            if id >= self.inner.len() {
                self.inner.resize(id + 1, None);
            }
            None
        };
        self.mask.add(index);
        self.inner[index as usize] = Some(component);
        prev
    }

    fn remove(&mut self, index: &u32) -> Option<Self::Component> {
        let index = *index;
        let id = index as usize;
        if self.mask.contains(index) {
            self.mask.remove(index);
            self.inner.remove(id)
        } else {
            None
        }
    }

    fn iter(&self) -> StorageIter<'_, Self::Component> {
        let iter = (0..).zip(self.inner.iter().map(|p| p.as_ref()));
        StorageIter {
            iter: Box::new(iter),
        }
    }

    fn masked_iter<M: BitSetLike + 'static>(
        &self,
        mask: M,
    ) -> JoinIter<'_, Option<&Self::Component>> {
        let iter = (0..)
            .zip(self.inner.iter().map(|p| p.as_ref()))
            .map(move |(n, mt)| if mask.contains(n) { mt } else { None });
        JoinIter {
            iter: Box::new(iter),
        }
    }

    fn iter_mut(&mut self) -> StorageIterMut<'_, Self::Component> {
        let iter = (0..).zip(self.inner.iter_mut().map(|p| p.as_mut()));
        StorageIterMut {
            iter: Box::new(iter),
        }
    }

    fn masked_iter_mut<M: BitSetLike + 'static>(
        &mut self,
        mask: M,
    ) -> JoinIter<'_, Option<&mut Self::Component>> {
        let iter = (0..)
            .zip(self.inner.iter_mut().map(|p| p.as_mut()))
            .map(move |(n, mt)| if mask.contains(n) { mt } else { None });
        JoinIter {
            iter: Box::new(iter),
        }
    }
}

pub struct Entity {
    id: u32,
}

impl Deref for Entity {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.id
    }
}

impl Entity {
    pub fn id(&self) -> u32 {
        self.id
    }
}

#[derive(Default)]
pub struct Entities {
    next_k: u32,
    alive_set: BitSet,
    dead: VecDeque<Entity>,
}

impl Entities {
    pub fn new() -> Self {
        Entities {
            next_k: 0,
            alive_set: BitSet::new(),
            dead: VecDeque::default(),
        }
    }

    pub fn create(&mut self) -> Entity {
        let id = self.next_k;
        self.next_k += 1;
        self.alive_set.add(id);
        Entity { id }
    }

    pub fn destroy(&mut self, entity: Entity) {
        self.alive_set.remove(entity.id);
        self.dead.push_front(entity);
    }
}

pub struct Without<T>(T);

pub trait StaticMask: BitSetLike + Clone + 'static {}
impl<T: BitSetLike + Clone + 'static> StaticMask for T {}

pub struct Maybe<T>(pub T);

pub trait MaybeJoin: Sized {
    fn maybe(&self) -> Maybe<&Self>;
    fn maybe_mut(&mut self) -> Maybe<&mut Self>;
}

impl<T: Storage> MaybeJoin for T {
    fn maybe(&self) -> Maybe<&Self> {
        Maybe(self)
    }

    fn maybe_mut(&mut self) -> Maybe<&mut Self> {
        Maybe(self)
    }
}

pub trait Join: Sized {
    type Mask: StaticMask;
    type Iter: Iterator;

    fn get_mask(&self) -> Self::Mask;

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter;

    fn join(self) -> Self::Iter {
        let mask = self.get_mask();
        self.get_masked_iter(mask)
    }
}

impl<'a> Join for &'a Entities {
    type Mask = BitSet;
    type Iter = Box<dyn Iterator<Item = Entity>>;

    fn get_mask(&self) -> Self::Mask {
        self.alive_set.clone()
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        let self_mask = self.alive_set.clone();
        Box::new(mask.iter().map(move |id| {
            assert!(self_mask.contains(id));
            Entity { id }
        }))
    }
}

impl<'a, T: Storage> Join for &'a T {
    type Mask = BitSet;
    type Iter = JoinIter<'a, &'a T::Component>;

    fn get_mask(&self) -> Self::Mask {
        self.mask().clone()
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        JoinIter::new(self.masked_iter(mask).filter_map(|p| p))
    }
}

impl<'a, T: Storage> Join for &'a mut T {
    type Mask = BitSet;
    type Iter = JoinIter<'a, &'a mut T::Component>;

    fn get_mask(&self) -> BitSet {
        self.mask().clone()
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        JoinIter::new(self.masked_iter_mut(mask).filter_map(|p| p))
    }
}

impl<'a, 'b: 'a, T: IsResource + Storage> Join for &'a mut Write<'b, T> {
    type Mask = BitSet;
    type Iter = JoinIter<'a, &'a mut T::Component>;

    fn get_mask(&self) -> BitSet {
        self.mask().clone()
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        JoinIter::new(self.masked_iter_mut(mask).filter_map(|p| p))
    }
}

impl<'a, 'b: 'a, T: IsResource + Storage> Join for &'a Read<'b, T> {
    type Mask = BitSet;
    type Iter = JoinIter<'a, &'a T::Component>;

    fn get_mask(&self) -> BitSet {
        self.mask().clone()
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        JoinIter::new(self.masked_iter(mask).filter_map(|p| p))
    }
}

impl<'a, T: Storage> Join for Maybe<&'a T> {
    type Mask = BitSetAll;
    type Iter = JoinIter<'a, Option<&'a T::Component>>;

    fn get_mask(&self) -> Self::Mask {
        BitSetAll
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        JoinIter::new(self.0.masked_iter(mask))
    }
}

impl<'a, T: Storage> Join for Maybe<&'a mut T> {
    type Mask = BitSetAll;
    type Iter = JoinIter<'a, Option<&'a mut T::Component>>;

    fn get_mask(&self) -> Self::Mask {
        BitSetAll
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        JoinIter::new(self.0.masked_iter_mut(mask))
    }
}

impl<T: Join> Join for Without<T> {
    type Mask = BitSetNot<T::Mask>;
    type Iter = Cycle<Once<()>>;

    fn get_mask(&self) -> Self::Mask {
        let bitset = self.0.get_mask();
        BitSetNot(bitset)
    }

    fn get_masked_iter<M: StaticMask>(self, _: M) -> Self::Iter {
        std::iter::once(()).cycle()
    }
}

impl<A: Join> Join for (A,) {
    type Mask = A::Mask;
    type Iter = A::Iter;

    fn get_mask(&self) -> Self::Mask {
        self.0.get_mask()
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        self.0.get_masked_iter(mask)
    }
}

impl<A: Join, B: Join> Join for (A, B) {
    type Mask = BitSetAnd<A::Mask, B::Mask>;
    type Iter = Zip<(<A as Join>::Iter, <B as Join>::Iter)>;

    fn get_mask(&self) -> Self::Mask {
        let a = self.0.get_mask();
        let b = self.1.get_mask();
        BitSetAnd(a, b)
    }

    fn get_masked_iter<M: StaticMask>(self, mask: M) -> Self::Iter {
        let a_iter = self.0.get_masked_iter(mask.clone());
        let b_iter = self.1.get_masked_iter(mask);
        multizip((a_iter, b_iter))
    }
}

apecs_derive::define_join_tuple!((A, B, C));
apecs_derive::define_join_tuple!((A, B, C, D));
apecs_derive::define_join_tuple!((A, B, C, D, E));
apecs_derive::define_join_tuple!((A, B, C, D, E, F));
apecs_derive::define_join_tuple!((A, B, C, D, E, F, G));
apecs_derive::define_join_tuple!((A, B, C, D, E, F, G, H));
apecs_derive::define_join_tuple!((A, B, C, D, E, F, G, H, I));
apecs_derive::define_join_tuple!((A, B, C, D, E, F, G, H, I, J));
apecs_derive::define_join_tuple!((A, B, C, D, E, F, G, H, I, J, K));
apecs_derive::define_join_tuple!((A, B, C, D, E, F, G, H, I, J, K, L));

#[cfg(test)]
mod test {
    use super::*;
    use hibitset::{BitSet, BitSetLike};

    fn make_abc_vecstorage() -> VecStorage<String> {
        let mut vs = VecStorage::new();
        assert!(vs.insert(0, "abc".to_string()).is_none());
        assert!(vs.insert(1, "def".to_string()).is_none());
        assert!(vs.insert(2, "hij".to_string()).is_none());
        assert!(vs.insert(10, "666".to_string()).is_none());

        vs
    }

    fn make_2468_vecstorage() -> VecStorage<i32> {
        let mut vs = VecStorage::new();
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
        let mut strings: VecStorage<String> = VecStorage::new();
        let mut numbers: VecStorage<u32> = VecStorage::new();

        let a = entities.create();
        strings.insert(a.id(), "A".to_string());
        numbers.insert(a.id(), 1);

        let b = entities.create();
        strings.insert(b.id(), "B".to_string());
        numbers.insert(b.id(), 2);

        let c = entities.create();
        strings.insert(c.id(), "C".to_string());

        for (entity, mut s, n) in (&entities, &mut strings, &numbers).join() {
            *s = format!("{}{}", s, entity.id());
        }

        assert_eq!(strings.get(&a), Some(&"A0".to_string()));
        assert_eq!(strings.get(&b), Some(&"B1".to_string()));
        assert_eq!(strings.get(&c), Some(&"C".to_string()));
    }

    #[test]
    fn bitset_sanity() {
        let a = BitSet::from_iter([1, 2, 3]);
        let b = BitSet::from_iter([4, 5, 6]);
        let c = BitSet::from_iter([1, 2, 3, 4, 5, 6]);
        assert!(c.contains_set(&a));
        assert!(c.contains_set(&b));

        let b_inv = !b;
        assert!(b_inv.contains(1));
        assert!(b_inv.contains(2));
        assert!(b_inv.contains(3));
    }

    #[test]
    fn can_join_maybe() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        let mut joined = (&vs_abc, vs_246.maybe()).join();
        assert_eq!(joined.next().unwrap(), (&"abc".to_string(), Some(&0)));
        assert_eq!(joined.next().unwrap(), (&"def".to_string(), None));
        assert_eq!(joined.next().unwrap(), (&"hij".to_string(), Some(&2)));
        assert_eq!(joined.next().unwrap(), (&"666".to_string(), None));
        assert_eq!(joined.next(), None);
    }

    #[test]
    fn can_join_without() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        let mut joined = (&vs_abc, !&vs_246).join();
        assert_eq!(joined.next(), Some((&"def".to_string(), ())));
        assert_eq!(joined.next(), None);
    }

    #[test]
    fn can_join_a_b() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        let mut iter = (&vs_abc, &vs_246).join();
        assert_eq!(iter.next(), Some((&"abc".to_string(), &0)));
        assert_eq!(iter.next(), Some((&"hij".to_string(), &2)));
        assert_eq!(iter.next(), Some((&"666".to_string(), &10)));
    }

    #[test]
    fn can_iter_mut() {
        let mut vs = make_abc_vecstorage();
        let mut iter = vs.iter_mut().flat_map(|(n, mt)| mt.map(|t| (n, t)));

        let (_, abc) = iter.next().unwrap();
        assert_eq!(abc.as_str(), "abc");

        let (_, def) = iter.next().unwrap();
        assert_eq!(def.as_str(), "def");

        let (_, hij) = iter.next().unwrap();
        assert_eq!(hij.as_str(), "hij");

        let (_, beast) = iter.next().unwrap();
        assert_eq!(beast.as_str(), "666");

        assert!(iter.next().is_none());
    }

    #[test]
    fn can_masked_iterate() {
        let mut vs = make_abc_vecstorage();
        let mut mask = BitSet::new();
        mask.add(0);
        mask.add(10);
        mask.add(2);

        fn run_test<T: AsRef<str>>(iter: &mut impl Iterator<Item = Option<T>>) {
            let abc = iter.next().unwrap().unwrap();
            assert_eq!(abc.as_ref(), "abc");

            assert!(iter.next().unwrap().is_none());

            let hij = iter.next().unwrap().unwrap();
            assert_eq!(hij.as_ref(), "hij");

            for _ in 3..10 {
                assert!(iter.next().unwrap().is_none());
            }

            let beast = iter.next().unwrap().unwrap();
            assert_eq!(beast.as_ref(), "666");

            assert!(iter.next().is_none());
        }

        let mut iter = vs.masked_iter_mut(mask.clone());
        run_test(&mut iter);
        drop(iter);

        let mut iter = vs.masked_iter(mask);
        run_test(&mut iter);
    }
}
