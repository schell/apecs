use std::iter::Map;

use hibitset::{BitIter, BitSet};
pub use rayon::iter::{
    FilterMap, IndexedParallelIterator, IntoParallelIterator, MultiZip, ParallelIterator, Zip,
};
use tuple_list::{Tuple, TupleList};

use crate::{
    storage::{separate::*, Entry, IsEntry},
    world::Entities,
};

#[inline]
pub fn sync<A, B>(a: &mut A, next_a: &mut A::Item, b: &mut B, next_b: &mut B::Item) -> Option<()>
where
    A: Iterator,
    A::Item: IsEntry,
    B: Iterator,
    B::Item: IsEntry,
{
    while next_a.id() != next_b.id() {
        while next_a.id() < next_b.id() {
            *next_a = a.next()?;
        }
        while next_b.id() < next_a.id() {
            *next_b = b.next()?;
        }
    }

    Some(())
}

pub struct JoinedIter<T>(T);

pub trait IntoJoinIterator {
    type Iter: Iterator;

    fn join_iter(self) -> Self::Iter;
}

impl<'a, T: CanWriteStorage> IntoJoinIterator for &'a mut T {
    type Iter = T::IterMut<'a>;

    fn join_iter(self) -> Self::Iter {
        self.iter_mut()
    }
}

impl<'a, T: CanReadStorage> IntoJoinIterator for &'a T {
    type Iter = T::Iter<'a>;

    fn join_iter(self) -> Self::Iter {
        self.iter()
    }
}

impl<'a, T> IntoJoinIterator for std::slice::Iter<'a, T>
where
    T: IsEntry,
{
    type Iter = Self;

    fn join_iter(self) -> Self::Iter {
        self
    }
}

impl<'a, T> IntoJoinIterator for std::slice::IterMut<'a, T>
where
    T: IsEntry,
{
    type Iter = Self;

    fn join_iter(self) -> Self::Iter {
        self
    }
}

impl<T> IntoJoinIterator for Vec<T>
where
    T: IsEntry,
{
    type Iter = std::vec::IntoIter<T>;

    fn join_iter(self) -> Self::Iter {
        self.into_iter()
    }
}

impl<'a> IntoJoinIterator for &'a Entities {
    type Iter = Map<BitIter<&'a BitSet>, fn(u32) -> usize>;

    fn join_iter(self) -> Self::Iter {
        self.iter()
    }
}

impl<C, T> IntoJoinIterator for Without<T>
where
    C: IsEntry,
    T: IntoJoinIterator,
    T::Iter: Iterator<Item = C>,
{
    type Iter = WithoutIter<T::Iter>;

    fn join_iter(self) -> Self::Iter {
        WithoutIter::new(self.0.join_iter())
    }
}

impl<T, C> IntoJoinIterator for MaybeIter<C, T>
where
    C: IsEntry,
    T: Iterator<Item = C>,
{
    type Iter = Self;

    fn join_iter(self) -> Self::Iter {
        self
    }
}

impl<Head: IntoJoinIterator> IntoJoinIterator for (Head, ()) {
    type Iter = Head::Iter;

    fn join_iter(self) -> Self::Iter {
        self.0.join_iter()
    }
}

impl<Head, Next, Tail> IntoJoinIterator for (Head, (Next, Tail))
where
    Head: IntoJoinIterator,
    (Next, Tail): IntoJoinIterator + TupleList,
    <Head::Iter as Iterator>::Item: IsEntry,
    <<(Next, Tail) as IntoJoinIterator>::Iter as Iterator>::Item: IsEntry,
    (
        <<Head as IntoJoinIterator>::Iter as Iterator>::Item,
        <<(Next, Tail) as IntoJoinIterator>::Iter as Iterator>::Item,
    ): TupleList,
{
    type Iter = JoinedIter<(Head::Iter, <(Next, Tail) as IntoJoinIterator>::Iter)>;

    fn join_iter(self) -> Self::Iter {
        JoinedIter((self.0.join_iter(), self.1.join_iter()))
    }
}

impl<Head, Tail> Iterator for JoinedIter<(Head, Tail)>
where
    Head: Iterator,
    Tail: Iterator,
    <Head as Iterator>::Item: IsEntry,
    <Tail as Iterator>::Item: IsEntry,
    (Head::Item, Tail::Item): TupleList,
{
    type Item = <(Head::Item, Tail::Item) as TupleList>::Tuple;

    fn next(&mut self) -> Option<Self::Item> {
        let mut head = self.0 .0.next()?;
        let mut tail = self.0 .1.next()?;
        sync(&mut self.0 .0, &mut head, &mut self.0 .1, &mut tail)?;
        Some((head, tail).into_tuple())
    }
}

pub trait Join {
    type Iter: Iterator;
    fn join(self) -> Self::Iter;
}

impl<T> Join for T
where
    T: Tuple,
    T::TupleList: IntoJoinIterator,
{
    type Iter = <T::TupleList as IntoJoinIterator>::Iter;

    fn join(self) -> Self::Iter {
        self.into_tuple_list().join_iter()
    }
}

#[cfg(test)]
mod test {
    use std::ops::Deref;

    use crate::storage::separate::join::*;

    #[test]
    fn can_par_join() {
        let mut store_a = VecStorage::new_with_capacity(1000);
        let mut store_b = VecStorage::new_with_capacity(1000);
        let mut store_c = VecStorage::new_with_capacity(1000);
        let mut check = vec![];

        for i in 0..1000 {
            if i % 2 == 0 {
                store_a.insert(i, i);
            }
            if i % 3 == 0 {
                store_b.insert(i, i);
            }
            if i % 4 == 0 {
                store_c.insert(i, i);
            }
            if i % 2 == 0 && i % 3 == 0 && i % 4 == 0 {
                check.push((i, i, i));
            }
        }

        let result = (&store_a, &store_b, &store_c)
            .par_join()
            .map(|(a, b, c)| (*a.deref(), *b.deref(), *c.deref()))
            .collect::<Vec<_>>();

        assert_eq!(check, result);

        let _ = (&store_a, &store_b).par_join().collect::<Vec<_>>();
    }

    #[test]
    fn can_join_entities() {
        let entities = Entities::default();
        let abc_store: VecStorage<String> = VecStorage::default();
        for (_, _) in (&entities, &abc_store).join() {}
    }

    #[test]
    fn can_join_without() {
        let vs_abc = crate::storage::separate::test::make_abc_vecstorage();
        let vs_246 = crate::storage::separate::test::make_2468_vecstorage();

        let mut joined = (&vs_abc, !&vs_246).join();
        let (s, _) = joined.next().unwrap();
        assert_eq!(s.id(), 1);
        assert_eq!(s.as_str(), "def");

        assert_eq!(joined.next(), None);

        let mut vs_a: VecStorage<&str> = VecStorage::default();
        vs_a.insert(0, "a0");
        vs_a.insert(1, "a1");
        vs_a.insert(3, "a3");
        vs_a.insert(4, "a4");
        vs_a.insert(8, "a8");

        let withouts = (!&vs_a,).join().take(5).map(|e| e.id()).collect::<Vec<_>>();
        assert_eq!(&withouts, &[2, 5, 6, 7, 9]);
    }

    #[test]
    fn can_join_maybe() {
        let mut vs: VecStorage<&str> = VecStorage::default();
        vs.insert(1, "1");
        vs.insert(2, "2");
        vs.insert(3, "3");
        vs.insert(6, "6");

        let maybes = (vs.maybe(),)
            .join()
            .take(7)
            .map(|Maybe { key, inner }| (key, inner.map(|e| e.value())))
            .collect::<Vec<_>>();
        assert_eq!(
            &maybes,
            &[
                (0usize, None),
                (1, Some(&"1")),
                (2, Some(&"2")),
                (3, Some(&"3")),
                (4, None),
                (5, None),
                (6, Some(&"6"))
            ]
        );
    }

    #[test]
    fn decide_bool_or_option_unit_by_size() {
        let o = std::mem::size_of::<Option<()>>();
        let b = std::mem::size_of::<bool>();
        assert!(o == b, "{} bytes", o);
    }
}
