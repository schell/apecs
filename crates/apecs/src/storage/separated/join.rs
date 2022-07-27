//! Joining tuples of storages.
//!
//! Joining is to separate storages as querying is to archetypal storage.

use std::cmp::Ordering;

use rayon::iter::{IntoParallelIterator, ParallelIterator};
use tuple_list::{Tuple, TupleList};

use crate::storage::{Entry, HasEntityInfo, Maybe, HasId};

/// Converts a `TupleList` of `IntoIterator`s into a `TupleList` of `Iterator`s.
pub trait TupleListIntoIter {
    type Output;

    fn tuple_list_into_iter(self) -> Self::Output;
}

impl TupleListIntoIter for () {
    type Output = ();

    fn tuple_list_into_iter(self) -> Self::Output {
        ()
    }
}

impl<Head, Tail> TupleListIntoIter for (Head, Tail)
where
    Head: IntoIterator,
    Tail: TupleListIntoIter + TupleList,
{
    type Output = (Head::IntoIter, Tail::Output);

    fn tuple_list_into_iter(self) -> Self::Output {
        (self.0.into_iter(), self.1.tuple_list_into_iter())
    }
}

pub trait CompareId {
    fn cmp_id(&self, id: &usize) -> Ordering;
}

impl CompareId for () {
    fn cmp_id(&self, _: &usize) -> Ordering {
        Ordering::Equal
    }
}

impl CompareId for usize {
    fn cmp_id(&self, id: &usize) -> Ordering {
        self.cmp(id)
    }
}

impl<T> CompareId for Entry<T> {
    fn cmp_id(&self, id: &usize) -> Ordering {
        self.info.key.cmp(id)
    }
}

impl<T> CompareId for &Entry<T> {
    fn cmp_id(&self, id: &usize) -> Ordering {
        self.info.key.cmp(id)
    }
}

impl<T> CompareId for &mut Entry<T> {
    fn cmp_id(&self, id: &usize) -> Ordering {
        self.info.key.cmp(id)
    }
}

impl<T> CompareId for Maybe<T> {
    fn cmp_id(&self, id: &usize) -> Ordering {
        self.key.cmp(id)
    }
}

impl<A: CompareId, B> CompareId for (A, B) {
    fn cmp_id(&self, id: &usize) -> Ordering {
        self.0.cmp_id(id)
    }
}

pub trait TupleListSyncIter {
    type Item;

    fn next_item(&mut self) -> Option<Self::Item>;
}

impl TupleListSyncIter for () {
    type Item = ();

    #[inline]
    fn next_item(&mut self) -> Option<Self::Item> {
        Some(())
    }
}

impl<Head, Tail> TupleListSyncIter for (Head, Tail)
where
    Head: Iterator,
    Head::Item: HasId,
    Tail: TupleListSyncIter + TupleList,
    <Tail as TupleListSyncIter>::Item: CompareId,
{
    type Item = (<Head as Iterator>::Item, <Tail as TupleListSyncIter>::Item);

    #[inline]
    fn next_item(&mut self) -> Option<Self::Item> {
        let mut head = self.0.next()?;
        let mut tail = self.1.next_item()?;
        while tail.cmp_id(&head.id()) != Ordering::Equal {
            while tail.cmp_id(&head.id()) == Ordering::Greater {
                head = self.0.next()?;
            }
            while tail.cmp_id(&head.id()) == Ordering::Less {
                tail = self.1.next_item()?;
            }
        }
        Some((head, tail))
    }
}

pub struct JoinIter<T>(T);

impl<T> Iterator for JoinIter<T>
where
    T: TupleListSyncIter,
{
    type Item = <T as TupleListSyncIter>::Item;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next_item()
    }
}

pub type JoinIterList<T> = <<T as Tuple>::TupleList as TupleListIntoIter>::Output;
pub type JoinIterItem<T> = <JoinIter<JoinIterList<T>> as Iterator>::Item;
pub type JoinIterItemTuple<T> = <<JoinIter<<<T as Tuple>::TupleList as TupleListIntoIter>::Output> as Iterator>::Item as TupleList>::Tuple;

pub trait Join: Tuple
where
    Self::TupleList: TupleListIntoIter,
    JoinIterList<Self>: TupleListSyncIter,
    JoinIterItem<Self>: TupleList,
{
    fn join(
        self,
    ) -> std::iter::Map<
        JoinIter<JoinIterList<Self>>,
        fn(JoinIterItem<Self>) -> JoinIterItemTuple<Self>,
    > {
        let tlist_of_into_iters: Self::TupleList = self.into_tuple_list();
        let tlist_of_iters: JoinIterList<Self> = tlist_of_into_iters.tuple_list_into_iter();
        let tlist_sync_iter: JoinIter<JoinIterList<Self>> = JoinIter(tlist_of_iters);
        tlist_sync_iter.map(|tlist| tlist.into_tuple())
    }
}

impl<T> Join for T
where
    T: Tuple,
    T::TupleList: TupleListIntoIter,
    JoinIterList<T>: TupleListSyncIter,
    JoinIterItem<T>: TupleList,
{
}

pub trait TupleOfOptions {
    type Output: TupleList;

    fn into_option_of_tuple(self) -> Option<Self::Output>;
}

impl TupleOfOptions for () {
    type Output = ();

    fn into_option_of_tuple(self) -> Option<Self::Output> {
        Some(())
    }
}

impl<Head, Tail> TupleOfOptions for (Option<Head>, Tail)
where
    (Head, Tail::Output): TupleList,
    Tail: TupleOfOptions + TupleList,
{
    type Output = (Head, Tail::Output);

    #[inline]
    fn into_option_of_tuple(self) -> Option<Self::Output> {
        let head = self.0?;
        let tail = self.1.into_option_of_tuple()?;
        Some((head, tail))
    }
}

pub trait ParJoin: Sized + IntoParallelIterator
where
    <Self as IntoParallelIterator>::Item: Tuple,
    <<Self as IntoParallelIterator>::Item as Tuple>::TupleList: TupleOfOptions,
    <<<<Self as IntoParallelIterator>::Item as Tuple>::TupleList as TupleOfOptions>::Output as TupleList>::Tuple: Send,
{
    fn par_join(
        self,
    ) -> rayon::iter::FilterMap<
        <Self as IntoParallelIterator>::Iter,
        fn(
            <Self as IntoParallelIterator>::Item,
        ) -> Option<
            <<<<Self as IntoParallelIterator>::Item as Tuple>::TupleList as TupleOfOptions>::Output as TupleList>::Tuple,
        >,
    > {
        let multizip: <Self as IntoParallelIterator>::Iter = self.into_par_iter();
        multizip.filter_map(|tuple_of_options| {
            Some(tuple_of_options.into_tuple_list().into_option_of_tuple()?.into_tuple())
        })
    }
}

impl<T> ParJoin for T
where
    Self: Sized + IntoParallelIterator,
<Self as IntoParallelIterator>::Item: Tuple,
<<Self as IntoParallelIterator>::Item as Tuple>::TupleList: TupleOfOptions,
<<<<Self as IntoParallelIterator>::Item as Tuple>::TupleList as TupleOfOptions>::Output as TupleList>::Tuple: Send,
{
}

#[cfg(test)]
mod test {
    use crate::storage::separated::*;
    use rayon::prelude::*;

    #[test]
    fn join_tuple_list_iters() {
        let u32s = vec![(0usize, 0u32), (1, 1), (2, 2), (3, 3), (6, 6)];
        let strs = vec![(0usize, "zero"), (2, "two"), (3, "three"), (6, "six")];
        let evns = vec![(0usize, true), (1, false), (3, false), (4, true), (6, true)];
        let f32s = vec![(1, 1.0f32), (3, 3.0), (4, 4.0), (6, 6.0)];
        let (joined, ids): (Vec<_>, Vec<_>) = (u32s, strs, evns, f32s)
            .join()
            .map(|(u, s, b, f)| ((u.1, s.1, b.1, f.1), (u.0, s.0, b.0, f.0)))
            .unzip();
        let mut join_iter = joined.into_iter();
        assert_eq!((3u32, "three", false, 3.0), join_iter.next().unwrap());
        assert_eq!((6u32, "six", true, 6.0), join_iter.next().unwrap());
        assert!(join_iter.next().is_none());
        assert_eq!(vec![(3, 3, 3, 3), (6, 6, 6, 6)], ids);
    }

    #[test]
    fn par_join_vec_sanity() {
        let mut u32s = VecStorage::<u32>::default();
        u32s.insert(0, 1);
        u32s.insert(0, 2);
        u32s.insert(0, 3);
        let mut f32s = VecStorage::<f32>::default();
        f32s.insert(0, 1.0);
        f32s.insert(0, 2.0);
        f32s.insert(0, 3.0);
        (&u32s, &f32s)
            .par_join()
            .for_each(|(u, f)| assert_eq!(*u.value() as f32, *f.value()));
    }
}
