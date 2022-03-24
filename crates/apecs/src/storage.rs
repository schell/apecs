//! Entity component storage traits.
mod vec;
pub use vec::*;

mod sparse;
pub use sparse::*;

use crate::IsComponent;

pub trait StorageComponent {
    type Component;

    fn split(self) -> (usize, Self::Component);

    fn id(&self) -> usize;
}

impl<A> StorageComponent for (usize, A) {
    type Component = A;

    fn split(self) -> (usize, Self::Component) {
        self
    }

    fn id(&self) -> usize {
        self.0
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
}

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
}

/// A storage that can only be read from
pub trait CanReadStorage {
    type Component;

    type Iter<'a>: Iterator<Item = Entry<&'a Self::Component>>
    where
        Self: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component>;

    /// Return an iterator over all entities and indices
    fn iter(&self) -> Self::Iter<'_>;
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
}

#[cfg(test)]
pub mod test {
    use crate::{entities::*, join::*, storage::*};

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
