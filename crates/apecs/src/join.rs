use std::iter::Map;

use hibitset::{BitIter, BitSet};
pub use rayon::iter::{
    FilterMap, IndexedParallelIterator, IntoParallelIterator, MultiZip, ParallelIterator, Zip,
};

use crate::{entities::*, storage::*};

pub trait ParJoin {
    type Iter: ParallelIterator;

    fn par_join(self) -> Self::Iter;
}

impl<StoreA, StoreB, IterA, IterB, A, B> ParJoin for (StoreA, StoreB)
where
    StoreA: IntoParallelIterator<Iter = IterA>,
    StoreB: IntoParallelIterator<Iter = IterB>,
    IterA: IndexedParallelIterator<Item = Option<A>>,
    IterB: IndexedParallelIterator<Item = Option<B>>,
    A: Send,
    B: Send,
{
    type Iter = FilterMap<MultiZip<(IterA, IterB)>, fn((Option<A>, Option<B>)) -> Option<(A, B)>>;

    fn par_join(self) -> Self::Iter {
        self.into_par_iter().filter_map(|(ma, mb)| {
            let a = ma?;
            let b = mb?;
            Some((a, b))
        })
    }
}

apecs_derive::impl_parjoin_tuple!((A, B, C));
apecs_derive::impl_parjoin_tuple!((A, B, C, D));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E, F));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E, F, G));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E, F, G, H));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E, F, G, H, I));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E, F, G, H, I, J));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E, F, G, H, I, J, K));
apecs_derive::impl_parjoin_tuple!((A, B, C, D, E, F, G, H, I, J, K, L));

#[inline(always)]
pub fn sync<A, B>(
    a: &mut A,
    next_a: &mut A::Item,
    b: &mut B,
    next_b: &mut B::Item,
) -> Option<()>
where
    A: Iterator,
    <A as Iterator>::Item: StorageComponent,
    B: Iterator,
    <B as Iterator>::Item: StorageComponent,
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

impl<A> Iterator for JoinedIter<(A,)>
where
    A: Iterator,
    <A as Iterator>::Item: StorageComponent,
{
    type Item = (
        usize,
        <<A as Iterator>::Item as StorageComponent>::Component,
    );

    fn next(&mut self) -> Option<Self::Item> {
        let next_a = self.0 .0.next()?.split();
        Some((next_a.0, next_a.1))
    }
}

pub trait Join {
    type Iter: Iterator;

    fn join(self) -> Self::Iter;
}

impl<'a, T: CanWriteStorage> Join for &'a mut T {
    type Iter = T::IterMut<'a>;

    fn join(self) -> Self::Iter {
        self.iter_mut()
    }
}

impl<'a, T: CanReadStorage> Join for &'a T {
    type Iter = T::Iter<'a>;

    fn join(self) -> Self::Iter {
        self.iter()
    }
}

impl<'a> Join for &'a Entities {
    type Iter = Map<BitIter<&'a BitSet>, fn(u32) -> Entry<Entity>>;

    fn join(self) -> Self::Iter {
        self.iter()
    }
}

impl<T> Join for WithoutIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
{
    type Iter = Self;

    fn join(self) -> Self::Iter {
        self
    }
}

impl<T> Join for MaybeIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
{
    type Iter = Self;

    fn join(self) -> Self::Iter {
        self
    }
}

impl<A: Join> Join for (A,)
where
    A::Iter: Iterator,
    <A::Iter as Iterator>::Item: StorageComponent,
{
    type Iter = JoinedIter<(A::Iter,)>;

    fn join(self) -> Self::Iter {
        JoinedIter((self.0.join(),))
    }
}

impl<A, B> Join for (A, B)
where
    A: Join,
    B: Join,
    A::Iter: Iterator,
    B::Iter: Iterator,
<A::Iter as Iterator>::Item: StorageComponent,
<B::Iter as Iterator>::Item: StorageComponent,
{
    type Iter = JoinedIter<(A::Iter, B::Iter)>;
    fn join(self) -> Self::Iter {
        JoinedIter((self.0.join(), self.1.join()))
    }
}

impl<A, B> Iterator for JoinedIter<(A, B)>
where
    A: Iterator,
    B: Iterator,
    <A as Iterator>::Item: StorageComponent,
    <B as Iterator>::Item: StorageComponent,
{
    type Item = (
        usize,
        <<A as Iterator>::Item as StorageComponent>::Component,
        <<B as Iterator>::Item as StorageComponent>::Component,
    );
    fn next(&mut self) -> Option<Self::Item> {
        let mut next_0 = self.0 .0.next()?;
        let mut next_1 = self.0 .1.next()?;
        sync(&mut self.0 .0, &mut next_0, &mut self.0 .1, &mut next_1)?;
        Some((next_0.id(), next_0.value(), next_1.value()))
    }
}

apecs_derive::impl_join_tuple!((A, B, C));
apecs_derive::impl_join_tuple!((A, B, C, D));
apecs_derive::impl_join_tuple!((A, B, C, D, E));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K, L));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K, L, M));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K, L, M, N));

#[cfg(test)]
mod test {
    use crate::join::*;

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
            .map(|(a, b, c)| (*a, *b, *c))
            .collect::<Vec<_>>();

        assert_eq!(check, result);

        let _ = (&store_a, &store_b).par_join().collect::<Vec<_>>();
    }

    #[test]
    fn can_join_entities() {
        let entities = Entities::default();
        let abc_store: VecStorage<String> = VecStorage::default();
        for (_, _, _) in (&entities, &abc_store).join() {}
    }

    #[test]
    fn can_join_without() {
        let vs_abc = crate::storage::test::make_abc_vecstorage();
        let vs_246 = crate::storage::test::make_2468_vecstorage();

        let mut joined = (&vs_abc, !&vs_246).join();
        assert_eq!(joined.next(), Some((1, &"def".to_string(), ())));
        assert_eq!(joined.next(), None);

        let mut vs_a: VecStorage<&str> = VecStorage::default();
        vs_a.insert(0, "a0");
        vs_a.insert(1, "a1");
        vs_a.insert(3, "a3");
        vs_a.insert(4, "a4");
        vs_a.insert(8, "a8");

        let withouts = (!&vs_a,).join().take(5).map(|e| e.0).collect::<Vec<_>>();
        assert_eq!(&withouts, &[2, 5, 6, 7, 9]);
    }

    #[test]
    fn can_join_maybe() {
        let mut vs: VecStorage<&str> = VecStorage::default();
        vs.insert(1, "1");
        vs.insert(2, "2");
        vs.insert(3, "3");
        vs.insert(6, "6");

        let maybes = (vs.maybe(),).join().take(7).collect::<Vec<_>>();
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
