//! Archetype queries.
use std::ops::Deref;
use std::{any::TypeId, marker::PhantomData};

use any_vec::{traits::*, AnyVec};
use anyhow::Context;
use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use rayon::iter::{
    IndexedParallelIterator, IntoParallelIterator, ParallelIterator,
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

use super::IsBundle;

/// A placeholder type for borrowing an entire column of components from
/// the world.
pub struct ComponentColumn<T>(PhantomData<T>);

pub trait IsQuery {
    /// Data that is read or write locked by performing this query.
    type LockedColumns<'a>;
    /// Data that can be used to append to columns. Typically vectors of
    /// component entries or bundles of vectors of component entries.
    type ExtensionColumns: 'static;
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

    /// Extend entries in the locked columns.
    ///
    /// This is for internal use only.
    fn extend_locked_columns<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        extension_columns: Self::ExtensionColumns,
        output_ids: Option<&mut Vec<usize>>,
    );

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
    type ExtensionColumns = ();
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

    fn extend_locked_columns<'a, 'b>(
        _: &'b mut Self::LockedColumns<'a>,
        (): Self::ExtensionColumns,
        _: Option<&mut Vec<usize>>,
    ) {
        panic!("cannot mutate a read-only lock");
    }

    #[inline]
    fn iter_mut<'a, 'b>(locked: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        locked.as_ref().map_or_else(
            || (&[]).into_iter(),
            |data| {
                data.downcast_ref::<Entry<T>>()
                    .with_context(|| {
                        format!("can't downcast to {}", std::any::type_name::<Entry<T>>())
                    })
                    .unwrap()
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
    type ExtensionColumns = Box<dyn Iterator<Item = Entry<T>>>;
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

    fn extend_locked_columns<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        extension_columns: Self::ExtensionColumns,
        output_ids: Option<&mut Vec<usize>>,
    ) {
        lock.as_mut().map(|guard| {
            let mut vs = guard.downcast_mut::<Entry<T>>().expect("can't downcast");
            if let Some(output_ids) = output_ids {
                for entry in extension_columns {
                    output_ids.push(entry.id());
                    vs.push(entry);
                }
            } else {
                for entry in extension_columns {
                    vs.push(entry);
                }
            }
        });
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

impl<A> IsQuery for (A,)
where
    A: IsQuery,
{
    type LockedColumns<'a> = A::LockedColumns<'a>;
    type ExtensionColumns = A::ExtensionColumns;
    type QueryResult<'a> = A::QueryResult<'a>;
    type ParQueryResult<'a> = A::ParQueryResult<'a>;
    type QueryRow<'a> = A::QueryRow<'a>;

    fn borrows() -> Vec<Borrow> {
        A::borrows()
    }

    #[inline]
    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        A::lock_columns(arch)
    }

    fn extend_locked_columns<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        ext: Self::ExtensionColumns,
        output_ids: Option<&mut Vec<usize>>,
    ) {
        A::extend_locked_columns(lock, ext, output_ids);
    }

    #[inline]
    fn iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        A::iter_mut(lock)
    }

    fn iter_one<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b> {
        A::iter_one(lock, index)
    }

    fn par_iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::ParQueryResult<'b> {
        A::par_iter_mut(lock)
    }
}

impl<A, B> IsQuery for (A, B)
where
    A: IsQuery,
    B: IsQuery,
{
    type LockedColumns<'a> = (A::LockedColumns<'a>, B::LockedColumns<'a>);
    type ExtensionColumns = (A::ExtensionColumns, B::ExtensionColumns);
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

    fn extend_locked_columns<'a, 'b>(
        (a, b): &'b mut Self::LockedColumns<'a>,
        (ea, eb): Self::ExtensionColumns,
        output_ids: Option<&mut Vec<usize>>,
    ) {
        A::extend_locked_columns(a, ea, output_ids);
        B::extend_locked_columns(b, eb, None);
    }

    #[inline]
    fn iter_mut<'a, 'b>((col_a, col_b): &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        A::iter_mut(col_a).zip(B::iter_mut(col_b))
    }

    fn iter_one<'a, 'b>(
        (col_a, col_b): &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b> {
        A::iter_one(col_a, index).zip(B::iter_one(col_b, index))
    }

    fn par_iter_mut<'a, 'b>(
        (col_a, col_b): &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
        A::par_iter_mut(col_a).zip(B::par_iter_mut(col_b))
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

impl AllArchetypes {
    pub fn extend<B: IsBundle>(&mut self, extension: <B::MutBundle as IsQuery>::ExtensionColumns)
    where
        B::MutBundle: IsQuery,
    {
        let types = B::EntryBundle::ordered_types().unwrap();
        let archetype_index;
        let mut arch;
        if let Some((i, a)) = self.get_archetype_mut(&types) {
            archetype_index = i;
            arch = a;
        } else {
            let new_arch = Archetype::new::<B>().unwrap();
            archetype_index = self.archetypes.len();
            self.archetypes.push(new_arch);
            arch = &mut self.archetypes[archetype_index];
        }
        let mut index_lookup: Vec<usize> = vec![];
        {
            let mut lock = <B::MutBundle as IsQuery>::lock_columns(arch);
            <B::MutBundle as IsQuery>::extend_locked_columns(
                &mut lock,
                extension,
                Some(&mut index_lookup),
            );
        }
        arch.index_lookup = index_lookup.clone();
        drop(arch);
        index_lookup.iter().enumerate().for_each(|(i, id)| {
            if id >= &self.entity_lookup.len() {
                self.entity_lookup.resize_with(id + 1, Default::default);
            }
            self.entity_lookup[*id] = Some((archetype_index, i));
        });
    }

    pub fn query<Q: IsQuery + 'static>(&mut self) -> QueryGuard<'_, Q> {
        QueryGuard(
            self.archetypes.iter().map(|a| Q::lock_columns(a)).collect(),
            self,
        )
    }
}

/// A query that is active.
pub struct QueryGuard<'a, Q: IsQuery + ?Sized>(Vec<Q::LockedColumns<'a>>, &'a AllArchetypes);

impl<'a, Q> QueryGuard<'a, Q>
where
    Q: IsQuery + ?Sized,
{
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
        self.1
            .entity_lookup
            .get(entity_id)
            .copied()
            .flatten()
            .map(|(archetype_index, component_index)| {
                let mut vs = Q::iter_one(&mut self.0[archetype_index], component_index)
                    .collect::<Vec<_>>();
                vs.pop().unwrap()
            })
    }

    pub fn par_iter_mut(
        &mut self,
    ) -> rayon::iter::Flatten<rayon::vec::IntoIter<<Q as IsQuery>::ParQueryResult<'_>>> {
        self.0
            .iter_mut()
            .map(|cols| Q::par_iter_mut(cols))
            .collect::<Vec<_>>()
            .into_par_iter()
            .flatten()
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

        let mut q = all.query::<&f32>();
        let sum = q.iter_mut().fold(0.0f32, |sum, f| sum + **f);
        assert_eq!(6.0, sum);
    }

    #[test]
    fn iter_types() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        println!("insert");
        let mut store = AllArchetypes::default();
        store.insert_bundle(0, (0.0f32, "zero".to_string(), false));
        store.insert_bundle(1, (1.0f32, "one".to_string(), false));
        store.insert_bundle(2, (2.0f32, "two".to_string(), false));

        {
            println!("query 1");
            let mut q = store.query::<&f32>();
            let f32s = q.iter_mut().map(|f| **f).collect::<Vec<_>>();
            let f32s_again = q.iter_mut().map(|f| **f).collect::<Vec<_>>();
            assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s);
            assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s_again);
        }

        {
            println!("query 2");
            let mut q = store.query::<(&mut bool, &f32)>();
            let units: Vec<()> = q
                .iter_mut()
                .enumerate()
                .map(|(i, (is_on, f))| {
                    **is_on = !**is_on;
                    assert!(**is_on);
                    assert!(**f as usize == i);
                })
                .collect::<Vec<_>>();
            assert_eq!(3, units.len());
        }

        {
            println!("query 3");
            let mut q = store.query::<(&String, &mut bool, &mut f32)>();
            println!("query 3 iteration");
            q.iter_mut().for_each(|(s, is_on, f)| {
                **is_on = true;
                **f = s.len() as f32;
            });
        }

        println!("query 4");
        let mut q = store.query::<(&bool, &f32, &String)>();
        q.iter_mut().for_each(|(is_on, f, s)| {
            assert!(**is_on);
            assert_eq!(**f, s.len() as f32, "{}.len() != {}", **s, **f);
        });
    }

    #[test]
    fn query_one() {
        let mut store = AllArchetypes::default();
        store.insert_bundle(0, (0.0f32, "zero".to_string(), false));
        store.insert_bundle(1, (1.0f32, "one".to_string(), false));
        store.insert_bundle(2, (2.0f32, "two".to_string(), false));

        {
            let mut q = store.query::<(&mut f32, &mut String, &bool)>();
            let (f, s, b) = q.find_one(1).unwrap();
            *f.deref_mut() = 666.0;
            **s = format!("blah {:?} {:?}", f.value(), b.value());
        }

        let mut q = store.query::<(&f32, &String, &bool)>();
        let vs = q
            .iter_mut()
            .map(|(f, s, b)| (f.id(), **f, s.to_string(), **b))
            .collect::<Vec<_>>();
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
