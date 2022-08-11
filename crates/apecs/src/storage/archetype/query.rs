//! Archetype queries.
use std::ops::Deref;
use std::{any::TypeId, marker::PhantomData};

use any_vec::{traits::*, AnyVec};
use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use rayon::iter::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator, ParallelIterator,
};

use crate as apecs;
use crate::{
    resource_manager::LoanManager,
    schedule::Borrow,
    storage::{
        archetype::{AllArchetypes, Archetype},
        Entry,
    },
    CanFetch, Read, ResourceId,
};

/// A placeholder type for borrowing an entire column of components from
/// the world.
pub struct ComponentColumn<T>(PhantomData<T>);

pub trait IsQuery {
    /// Data that is read or write locked by performing this query.
    type LockedColumns<'a>;
    /// The iterator that is produced by performing a query on _one_ archetype.
    type QueryResult<'a>: Iterator<Item = Self::QueryRow<'a>>;
    /// The parallel iterator that is produced by performing a query on _one_
    /// archetype.
    type ParQueryResult<'a>: ParallelIterator<Item = Self::QueryRow<'a>> + IndexedParallelIterator;
    /// The iterator item.
    type QueryRow<'a>: Send + Sync;

    fn borrows() -> Vec<Borrow>;

    /// Find and acquire a "lock" on the columns for reading or writing.
    fn lock_columns<'a>(arch: &'a Archetype) -> Self::LockedColumns<'a>;

    /// Create an iterator over the rows of the given columns.
    fn iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b>;

    /// Create an iterator over one row with the given index.
    fn iter_one<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b>;

    /// Create an iterator over the rows of the given columns.
    fn par_iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::ParQueryResult<'b>;
}

impl<'s, T: Send + Sync + 'static> IsQuery for &'s T {
    type LockedColumns<'a> = Option<RwLockReadGuard<'a, AnyVec<dyn Send + Sync + 'static>>>;
    type QueryResult<'a> = std::slice::Iter<'a, Entry<T>>;
    type ParQueryResult<'a> = rayon::slice::Iter<'a, Entry<T>>;
    type QueryRow<'a> = &'a Entry<T>;

    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<ComponentColumn<T>>(),
            is_exclusive: false,
        }]
    }

    #[inline]
    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        let ty = TypeId::of::<Entry<T>>();
        let i = arch.index_of(&ty)?;
        let data = arch.data[i].read();
        Some(data)
    }

    #[inline]
    fn iter_mut<'a, 'b>(locked: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        locked.as_ref().map_or_else(
            || (&[]).into_iter(),
            |data| {
                data.downcast_ref::<Entry<T>>()
                    .expect("can't downcast")
                    .into_iter()
            },
        )
    }

    #[inline]
    fn iter_one<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b> {
        lock.as_ref().map_or_else(
            || (&[]).into_iter(),
            |data| {
                data.downcast_ref::<Entry<T>>()
                    .expect("can't downcast")
                    .as_slice()[index..=index]
                    .into_iter()
            },
        )
    }

    /// Create an iterator over the rows of the given columns.
    fn par_iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::ParQueryResult<'b> {
        lock.as_ref().map_or_else(
            || (&[]).into_par_iter(),
            |data| {
                data.downcast_ref::<Entry<T>>()
                    .expect("can't downcast")
                    .as_slice()
                    .into_par_iter()
            },
        )
    }
}

impl<'s, T: Send + Sync + 'static> IsQuery for &'s mut T {
    type LockedColumns<'a> = Option<RwLockWriteGuard<'a, AnyVec<dyn Send + Sync + 'static>>>;
    type QueryResult<'a> = std::slice::IterMut<'a, Entry<T>>;
    type ParQueryResult<'a> = rayon::slice::IterMut<'a, Entry<T>>;
    type QueryRow<'a> = &'a mut Entry<T>;

    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<ComponentColumn<T>>(),
            is_exclusive: true,
        }]
    }

    #[inline]
    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        let ty = TypeId::of::<Entry<T>>();
        let i = arch.index_of(&ty)?;
        let data = arch.data[i].write();
        Some(data)
    }

    #[inline]
    fn iter_mut<'a, 'b>(locked: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        locked.as_mut().map_or_else(
            || (&mut []).into_iter(),
            |data| {
                data.downcast_mut::<Entry<T>>()
                    .expect("can't downcast")
                    .into_iter()
            },
        )
    }

    #[inline]
    fn iter_one<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b> {
        lock.as_mut().map_or_else(
            || (&mut []).into_iter(),
            |data| {
                (&mut data
                    .downcast_mut::<Entry<T>>()
                    .expect("can't downcast")
                    .as_mut_slice()[index..=index])
                    .into_iter()
            },
        )
    }

    /// Create an iterator over the rows of the given columns.
    fn par_iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::ParQueryResult<'b> {
        lock.as_mut().map_or_else(
            || (&mut []).into_par_iter(),
            |data| {
                data.downcast_mut::<Entry<T>>()
                    .expect("can't downcast")
                    .as_mut_slice()
                    .into_par_iter()
            },
        )
    }
}

impl<A, B> IsQuery for (A, B)
where
    A: IsQuery,
    B: IsQuery,
{
    type LockedColumns<'a> = (A::LockedColumns<'a>, B::LockedColumns<'a>);
    type QueryResult<'a> = std::iter::Zip<A::QueryResult<'a>, B::QueryResult<'a>>;
    type ParQueryResult<'a> = rayon::iter::Zip<A::ParQueryResult<'a>, B::ParQueryResult<'a>>;
    type QueryRow<'a> = (A::QueryRow<'a>, B::QueryRow<'a>);

    fn borrows() -> Vec<Borrow> {
        let mut bs = A::borrows();
        bs.extend(B::borrows());
        bs
    }

    #[inline]
    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        let a = A::lock_columns(arch);
        let b = B::lock_columns(arch);
        (a, b)
    }

    #[inline]
    fn iter_mut<'a, 'b>((col_a, col_b): &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        A::iter_mut(col_a).zip(B::iter_mut(col_b))
    }

    fn par_iter_mut<'a, 'b>(
        (col_a, col_b): &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
        A::par_iter_mut(col_a).zip(B::par_iter_mut(col_b))
    }

    fn iter_one<'a, 'b>(
        (col_a, col_b): &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b> {
        A::iter_one(col_a, index).zip(B::iter_one(col_b, index))
    }
}

apecs_derive::impl_isquery_tuple!((A, B, C));
apecs_derive::impl_isquery_tuple!((A, B, C, D));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E, F));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E, F, G));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E, F, G, H));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E, F, G, H, I));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E, F, G, H, I, J));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E, F, G, H, I, J, K));
apecs_derive::impl_isquery_tuple!((A, B, C, D, E, F, G, H, I, J, K, L));

impl Archetype {
    #[inline]
    pub fn for_each<Q: IsQuery + ?Sized>(&self, f: impl FnMut(Q::QueryRow<'_>)) {
        let mut cols = Q::lock_columns(self);
        Q::iter_mut(&mut cols).for_each(f);
    }

    #[inline]
    pub fn par_for_each<Q: IsQuery + ?Sized>(&self, f: impl Fn(Q::QueryRow<'_>) + Send + Sync) {
        let mut cols = Q::lock_columns(self);
        Q::par_iter_mut(&mut cols).for_each(f)
    }

    #[inline]
    pub fn fold<Q: IsQuery + ?Sized, T>(
        &self,
        init: T,
        f: impl FnMut(T, Q::QueryRow<'_>) -> T,
    ) -> T {
        let mut cols = Q::lock_columns(self);
        Q::iter_mut(&mut cols).fold(init, f)
    }

    pub fn visit_bundle<Q: IsQuery + 'static, A>(
        &self,
        entity_id: usize,
        f: impl FnOnce(Q::QueryRow<'_>) -> A,
    ) -> Option<A> {
        let index = self.entity_lookup.get(&entity_id)?;
        let mut cols = Q::lock_columns(self);
        let mut iter: Q::QueryResult<'_> = Q::iter_one(&mut cols, *index);
        let entry_bundle: Option<Q::QueryRow<'_>> = iter.next();
        entry_bundle.map(f)
    }
}

impl AllArchetypes {
    #[inline]
    pub fn for_each<Q>(&self, mut f: impl FnMut(Q::QueryRow<'_>))
    where
        Q: IsQuery + ?Sized,
    {
        self.archetypes
            .iter()
            .for_each(|arch| arch.for_each::<Q>(&mut f));
    }

    pub fn par_for_each<Q: IsQuery + ?Sized>(&self, f: impl Fn(Q::QueryRow<'_>) + Send + Sync) {
        self.archetypes
            .par_iter()
            .for_each(|arch| arch.par_for_each::<Q>(&f));
    }

    #[inline]
    pub fn fold<Q: IsQuery + ?Sized, T>(
        &self,
        mut init: T,
        mut f: impl FnMut(T, Q::QueryRow<'_>) -> T,
    ) -> T {
        for arch in self.archetypes.iter() {
            init = arch.fold::<Q, T>(init, &mut f);
        }
        init
    }

    pub fn visit_bundle<Q: IsQuery + 'static, A>(
        &self,
        entity_id: usize,
        mut f: impl FnMut(Q::QueryRow<'_>) -> A,
    ) -> Option<A> {
        for arch in self.archetypes.iter() {
            let result = arch.visit_bundle::<Q, A>(entity_id, &mut f);
            if result.is_some() {
                return result;
            }
        }
        None
    }

    pub fn query<Q: IsQuery + 'static>(&self) -> QueryGuard<'_, Q> {
        QueryGuard(self.archetypes.iter().map(|a| Q::lock_columns(a)).collect(), &self)
    }
}

/// A query that is active.
pub struct QueryGuard<'a, Q: IsQuery + ?Sized>(Vec<Q::LockedColumns<'a>>, &'a AllArchetypes);

impl<'a, Q: IsQuery + ?Sized> QueryGuard<'a, Q> {
    pub fn iter_mut<'b>(
        &mut self,
    ) -> std::iter::FlatMap<
        std::slice::IterMut<'_, Q::LockedColumns<'a>>,
        Q::QueryResult<'_>,
        for<'r> fn(&'r mut Q::LockedColumns<'a>) -> Q::QueryResult<'r>,
    > {
        self.0.iter_mut().flat_map(|cols| Q::iter_mut(cols))
    }

    pub fn find_one(&mut self, entity_id: usize) -> Option<Q::QueryRow<'_>> {
        for (arch, lock) in self.1.archetypes.iter().zip(self.0.iter_mut()) {
            if let Some(index) = arch.entity_lookup.get(&entity_id) {
                log::trace!("find_one {} is at {}", entity_id, index);
                let mut vs = Q::iter_one(lock, *index).collect::<Vec<_>>();
                log::trace!("  len {}", vs.len());
                return vs.pop();
            }
        }
        None
    }
}

/// A query that has been prepared over all archetypes.
pub struct Query<T>(
    Box<dyn Deref<Target = AllArchetypes> + Send + Sync + 'static>,
    PhantomData<T>,
)
where
    T: IsQuery + ?Sized;

impl<T> CanFetch for Query<T>
where
    T: IsQuery + Send + Sync + ?Sized,
{
    fn borrows() -> Vec<Borrow> {
        let mut bs = <T as IsQuery>::borrows();
        bs.extend(Read::<AllArchetypes>::borrows());
        bs
    }

    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self> {
        let all = Read::<AllArchetypes>::construct(loan_mngr)?;
        Ok(Query(Box::new(all), PhantomData))
    }
}

impl<Q> Query<Q>
where
    Q: IsQuery + ?Sized,
{
    /// Acquire a lock on the archetype columns needed to perform the query.
    pub fn lock(&self) -> QueryGuard<'_, Q> {
        QueryGuard(
            self.0
                .archetypes
                .iter()
                .map(|a| Q::lock_columns(a))
                .collect(),
            &self.0,
        )
    }
}

#[cfg(test)]
mod test {
    use std::ops::DerefMut;

    use super::*;

    #[test]
    fn can_query_archetype() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut arch = Archetype::new::<(f32,)>().unwrap();
        arch.insert_bundle(0, (0.0f32,)).unwrap();
        arch.insert_bundle(1, (1.0f32,)).unwrap();
        arch.insert_bundle(2, (2.0f32,)).unwrap();
        arch.insert_bundle(3, (3.0f32,)).unwrap();

        let mut lock = <&f32>::lock_columns(&arch);
        let iter = <&f32>::iter_mut(&mut lock);
        let result: f32 = iter.map(|f| *f.value()).sum();
        assert_eq!(6.0, result);
    }

    #[test]
    fn can_query_all() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut all = AllArchetypes::default();
        all.insert_bundle(0, (0.0f32, true));
        all.insert_bundle(1, (1.0f32, false, "hello"));
        all.insert_bundle(2, (2.0f32, true, ()));
        all.insert_bundle(3, (3.0f32, false));

        let mut sum = 0.0;
        all.for_each::<&f32>(|f| {
            sum += f.value();
        });
        assert_eq!(6.0, sum);
    }

    #[test]
    fn archetype_iter_types() {
        println!("insert");
        let mut store = AllArchetypes::default();
        store.insert_bundle(0, (0.0f32, "zero".to_string(), false));
        store.insert_bundle(1, (1.0f32, "one".to_string(), false));
        store.insert_bundle(2, (2.0f32, "two".to_string(), false));

        println!("query 1");
        let f32s = store.fold::<&f32, _>(vec![], |mut v, f| {
            v.push(**f);
            v
        });
        let f32s_again = store.fold::<&f32, _>(vec![], |mut v, f| {
            v.push(**f);
            v
        });
        assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s);
        assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s_again);

        println!("query 2");
        let mut i = 0;
        store.for_each::<(&mut bool, &f32)>(|(is_on, f)| {
            **is_on = !**is_on;
            assert!(**is_on);
            assert!(**f as usize == i);
            i += 1;
        });
        assert_eq!(3, i);

        println!("query 3");
        store.for_each::<(&String, &mut bool, &mut f32)>(|(s, is_on, f)| {
            **is_on = true;
            **f = s.len() as f32;
        });

        println!("query 4");
        store.for_each::<(&bool, &f32, &String)>(|(is_on, f, s)| {
            assert!(**is_on);
            assert_eq!(**f, s.len() as f32, "{}.len() != {}", **s, **f);
        })
    }

    #[test]
    fn query_one() {
        let mut store = AllArchetypes::default();
        store.insert_bundle(0, (0.0f32, "zero".to_string(), false));
        store.insert_bundle(1, (1.0f32, "one".to_string(), false));
        store.insert_bundle(2, (2.0f32, "two".to_string(), false));

        store.visit_bundle::<(&mut f32, &mut String, &bool), ()>(1, |(f, s, b)| {
            *f.deref_mut() = 666.0;
            **s = format!("blah {:?} {:?}", f.value(), b.value())
        });

        let vs = store.fold::<(&f32, &String, &bool), _>(vec![], |mut vs, (f, s, b)| {
            vs.push((f.id(), **f, s.to_string(), **b));
            vs
        });

        assert_eq!(
            vec![
                (0usize, 0.0f32, "zero".to_string(), false,),
                (1, 666.0, "blah 666.0 false".to_string(), false,),
                (2, 2.0, "two".to_string(), false,),
            ],
            vs
        );
    }

    #[test]
    fn sanity_query_iter() {
        let mut store = AllArchetypes::default();
        store.insert_bundle(0, (0.0f32, "zero".to_string(), false));
        store.insert_bundle(1, (1.0f32, "one".to_string(), false));
        store.insert_bundle(2, (2.0f32, "two".to_string(), false));

        type MyQuery<'a> = (&'a f32, &'a String, &'a bool);
        let mut locked = store
            .archetypes
            .iter()
            .map(|a| MyQuery::lock_columns(a))
            .collect::<Vec<_>>();
        let mut count = 0;
        let iter = locked.iter_mut().flat_map(|cols| MyQuery::iter_mut(cols));
        for (f, s, b) in iter {
            assert!(!**b, "{} {} {}", f.value(), s.value(), b.value());
            count += 1;
        }
        assert_eq!(3, count);
    }
}
