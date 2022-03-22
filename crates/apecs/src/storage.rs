//! Entity component storage trait and implementations.
use std::{
    iter::Map,
    ops::Deref,
    slice::{Iter, IterMut},
};

use hibitset::{BitIter, BitSet, BitSetLike};
pub use itertools::{multizip, Zip};

pub trait StorageComponent {
    type Component;

    fn split(self) -> (u32, Self::Component);

    fn id(&self) -> u32;
}

impl<A> StorageComponent for (u32, A) {
    type Component = A;

    fn split(self) -> (u32, Self::Component) {
        self
    }

    fn id(&self) -> u32 {
        self.0
    }
}

impl StorageComponent for u32 {
    type Component = u32;

    fn split(self) -> (u32, Self::Component) {
        (self, self)
    }

    fn id(&self) -> u32 {
        *self
    }
}

pub trait Storage: Sized {
    type Component;
    type Iter<'a>: Iterator<Item = (u32, &'a Self::Component)>
    where
        Self: 'a;
    type IterMut<'a>: Iterator<Item = (u32, &'a mut Self::Component)>
    where
        Self: 'a;

    fn new() -> Self;

    fn get(&self, id: &u32) -> Option<&Self::Component>;

    fn get_mut(&mut self, id: &u32) -> Option<&mut Self::Component>;

    fn insert(&mut self, id: u32, component: impl Into<Self::Component>)
        -> Option<Self::Component>;

    fn remove(&mut self, id: &u32) -> Option<Self::Component>;

    /// Return an iterator over all entities and indices
    fn iter(&self) -> Self::Iter<'_>;

    /// Return an iterator over entities, with the mutable components and their indices.
    fn iter_mut(&mut self) -> Self::IterMut<'_>;
}

pub struct VecStorage<T>{
    mask: BitSet,
    store: Vec<(u32, T)>
}

impl<T> Default for VecStorage<T> {
    fn default() -> Self {
        VecStorage{
            mask: BitSet::new(),
            store: vec![]
        }
    }
}

//impl<'a, T> std::ops::Not for &'a VecStorage<T> {
//    type Output = Without<Self>;
//
//    fn not(self) -> Self::Output {
//        Without(self)
//    }
//}

impl<T> Storage for VecStorage<T> {
    type Component = T;
    type Iter<'a> = Map<Iter<'a, (u32, T)>, fn(&(u32, T)) -> (u32, &T)> where T: 'a;
    type IterMut<'a> = Map<IterMut<'a, (u32, T)>, fn(&mut (u32, T)) -> (u32, &mut T)> where T: 'a;

    fn new() -> Self {
        Default::default()
    }

    fn get(&self, id: &u32) -> Option<&Self::Component> {
        if self.mask.contains(*id) {
            for (eid, t) in self.store.iter() {
                if eid == id {
                    return Some(t);
                } else if eid > id {
                    break;
                }
            }
        }
        None
    }

    fn get_mut(&mut self, id: &u32) -> Option<&mut Self::Component> {
        if self.mask.contains(*id) {
            for (eid, t) in self.store.iter_mut() {
                if eid == id {
                    return Some(t);
                } else if *eid > *id {
                    break;
                }
            }
        }
        None
    }

    fn insert(
        &mut self,
        id: u32,
        component: impl Into<Self::Component>,
    ) -> Option<Self::Component> {
        if self.mask.contains(id) {
            if let Some(t) = self.get_mut(&id) {
                return Some(std::mem::replace(t, component.into()));
            }
        }
        self.mask.add(id);
        self.store.push((id, component.into()));
        None
    }

    fn remove(&mut self, id: &u32) -> Option<Self::Component> {
        if self.mask.contains(*id) {
            self.mask.remove(*id);
            let mut found = None;
            for ((eid, _), i) in self.store.iter().zip(0..) {
                if eid == id {
                    found = Some(i);
                    break;
                }
            }
            found.map(|i| self.store.remove(i).1)
        } else {
            None
        }
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.store.iter().map(|(id, t)| (*id, t))
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        self.store.iter_mut().map(|(id, t)| (*id, t))
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
    pub(crate) next_k: u32,
    pub(crate) alive_set: BitSet,
}

impl Entities {
    pub fn new() -> Self {
        Entities {
            next_k: 0,
            alive_set: BitSet::new(),
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
    }

    pub fn iter(&self) -> Map<BitIter<&BitSet>, fn(u32) -> (usize, Entity)> {
        (&self.alive_set)
            .iter()
            .map(|id| (id as usize, Entity { id }))
    }
}

//pub struct Without<T>(T);
//
//pub struct Maybe<T>(pub T);
//
//pub trait MaybeJoin: Sized {
//    fn maybe(&self) -> Maybe<&Self>;
//    fn maybe_mut(&mut self) -> Maybe<&mut Self>;
//}
//
//impl<T: Storage> MaybeJoin for T {
//    fn maybe(&self) -> Maybe<&Self> {
//        Maybe(self)
//    }
//
//    fn maybe_mut(&mut self) -> Maybe<&mut Self> {
//        Maybe(self)
//    }
//}
//
//impl<'a, T: Storage> Join for Maybe<&'a T> {
//    type Iter = T::Iter<'a>;
//
//    fn join(self) -> JoinIter<Self::Iter> {
//        todo!()
//    }
//}
//
//impl<'a, T: Storage> Join for Maybe<&'a mut T> {}
//
//impl<T: Join> Join for Without<T> {}

#[cfg(test)]
mod test {
    use crate::{storage::*, join::*};
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
        numbers.insert(a.id(), 1u32);

        let b = entities.create();
        strings.insert(b.id(), "B".to_string());
        numbers.insert(b.id(), 2u32);

        let c = entities.create();
        strings.insert(c.id(), "C".to_string());

        for (_, entity, s, n) in (&entities, &mut strings, &numbers).join() {
            *s = format!("{}{}{}", s, entity.id(), n);
        }

        assert_eq!(strings.get(&a), Some(&"A01".to_string()));
        assert_eq!(strings.get(&b), Some(&"B12".to_string()));
        assert_eq!(strings.get(&c), Some(&"C".to_string()));
    }

    //#[test]
    //fn can_join_maybe() {
    //    let vs_abc = make_abc_vecstorage();
    //    let vs_246 = make_2468_vecstorage();

    //    let mut joined = (&vs_abc, vs_246.maybe()).join();
    //    assert_eq!(joined.next().unwrap(), (&"abc".to_string(), Some(&0)));
    //    assert_eq!(joined.next().unwrap(), (&"def".to_string(), None));
    //    assert_eq!(joined.next().unwrap(), (&"hij".to_string(), Some(&2)));
    //    assert_eq!(joined.next().unwrap(), (&"666".to_string(), None));
    //    assert_eq!(joined.next(), None);
    //}

    //#[test]
    //fn can_join_without() {
    //    let vs_abc = make_abc_vecstorage();
    //    let vs_246 = make_2468_vecstorage();

    //    let mut joined = (&vs_abc, !&vs_246).join();
    //    assert_eq!(joined.next(), Some((&"def".to_string(), ())));
    //    assert_eq!(joined.next(), None);
    //}

    //#[test]
    //fn can_join_a_b() {
    //    let vs_abc = make_abc_vecstorage();
    //    let vs_246 = make_2468_vecstorage();

    //    let mut iter = (&vs_abc, &vs_246).join();
    //    assert_eq!(iter.next(), Some((&"abc".to_string(), &0)));
    //    assert_eq!(iter.next(), Some((&"hij".to_string(), &2)));
    //    assert_eq!(iter.next(), Some((&"666".to_string(), &10)));
    //}

    //#[test]
    //fn can_masked_iterate() {
    //    let mut vs = make_abc_vecstorage();
    //    let mut mask = BitSet::new();
    //    mask.add(0);
    //    mask.add(10);
    //    mask.add(2);

    //    fn run_test<T: AsRef<str>>(iter: &mut impl Iterator<Item = Option<T>>) {
    //        let abc = iter.next().unwrap().unwrap();
    //        assert_eq!(abc.as_ref(), "abc");

    //        assert!(iter.next().unwrap().is_none());

    //        let hij = iter.next().unwrap().unwrap();
    //        assert_eq!(hij.as_ref(), "hij");

    //        for _ in 3..10 {
    //            assert!(iter.next().unwrap().is_none());
    //        }

    //        let beast = iter.next().unwrap().unwrap();
    //        assert_eq!(beast.as_ref(), "666");

    //        assert!(iter.next().is_none());
    //    }

    //    let mut iter = vs.masked_iter_mut(mask.clone());
    //    run_test(&mut iter);
    //    drop(iter);

    //    let mut iter = vs.masked_iter(mask);
    //    run_test(&mut iter);
    //}

    //#[test]
    //fn entities_create() {
    //    let mut entities = Entities::default();
    //    let a = entities.create();
    //    let b = entities.create();
    //    let c = entities.create();

    //    assert_eq!(a.id(), 0);
    //    assert_eq!(b.id(), 1);
    //    assert_eq!(c.id(), 2);
    //}

    //#[test]
    //fn entity_create_storage_insert() {
    //    #[derive(Clone)]
    //    struct A(f32);

    //    let mut entities = Entities::default();
    //    let mut a_storage: VecStorage<A> = VecStorage::default();
    //    let _ = (0..10000)
    //        .map(|_| {
    //            let e = entities.create();
    //            let _ = a_storage.insert(e.id(), A(0.0));
    //            e
    //        })
    //        .collect::<Vec<_>>();
    //}
}
