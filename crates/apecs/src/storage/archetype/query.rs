//! Component queries.
use std::ops::Deref;
use std::{any::TypeId, marker::PhantomData};

use any_vec::{traits::*, AnyVec};
use anyhow::Context;
use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use crate as apecs;
use crate::{
    resource_manager::LoanManager,
    internal::Borrow,
    storage::{
        archetype::{Archetype, Components},
        Entry,
    },
    CanFetch, Read, TypeKey,
};

use super::IsBundle;

/// A placeholder type for borrowing an entire column of components from
/// the world.
pub struct ComponentColumn<T>(PhantomData<T>);

/// Denotes the shape of a query that can be used to iterate over bundles of
/// components.
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
        output_ids: Option<(&mut Vec<usize>, &mut usize)>,
    );

    /// Create an iterator over the rows of the given columns.
    fn iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b>;

    /// Create an iterator over one row with the given index.
    fn iter_one<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b>;

    /// Create an iterator over the rows of the given columns.
    fn par_iter_mut<'a, 'b>(
        len: usize,
        lock: &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b>;
}

impl<'s, T: Send + Sync + 'static> IsQuery for &'s T {
    type LockedColumns<'a> = Option<RwLockReadGuard<'a, AnyVec<dyn Send + Sync + 'static>>>;
    type ExtensionColumns = ();
    type QueryResult<'a> = std::slice::Iter<'a, Entry<T>>;
    type ParQueryResult<'a> = rayon::slice::Iter<'a, Entry<T>>;
    type QueryRow<'a> = &'a Entry<T>;

    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: TypeKey::new::<ComponentColumn<T>>(),
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
        _: Option<(&mut Vec<usize>, &mut usize)>,
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
    fn par_iter_mut<'a, 'b>(
        _: usize,
        lock: &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
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
            id: TypeKey::new::<ComponentColumn<T>>(),
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
        output_ids: Option<(&mut Vec<usize>, &mut usize)>,
    ) {
        lock.as_mut().map(|guard| {
            let mut vs = guard.downcast_mut::<Entry<T>>().expect("can't downcast");
            let (lower, may_upper) = extension_columns.size_hint();
            let additional = may_upper.unwrap_or(lower);
            vs.reserve(additional);
            if let Some((output_ids, max_id)) = output_ids {
                for entry in extension_columns {
                    let id = entry.id();
                    *max_id = id.max(*max_id);
                    output_ids.push(id);
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
    fn par_iter_mut<'a, 'b>(
        _: usize,
        lock: &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
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

/// Query type used to denote an optional column.
///
/// Use `Maybe` in queries to return bundles of components that may or may not
/// contain a component of the wrapped type `T`.
pub struct Maybe<T>(PhantomData<T>);

impl<'s, T: Send + Sync + 'static> IsQuery for Maybe<&'s T> {
    type LockedColumns<'a> = Option<RwLockReadGuard<'a, AnyVec<dyn Send + Sync + 'static>>>;
    type ExtensionColumns = ();
    type QueryResult<'a> = itertools::Either<
        std::iter::RepeatWith<fn() -> Option<&'a Entry<T>>>,
        std::iter::Map<
            std::slice::Iter<'a, Entry<T>>,
            for<'r> fn(&'r Entry<T>) -> Option<&'r Entry<T>>,
        >,
    >;
    type ParQueryResult<'a> = rayon::iter::Either<
        rayon::iter::Map<rayon::iter::RepeatN<()>, fn(()) -> Option<&'a Entry<T>>>,
        rayon::iter::Map<
            rayon::slice::Iter<'a, Entry<T>>,
            for<'r> fn(&'r Entry<T>) -> Option<&'r Entry<T>>,
        >,
    >;
    type QueryRow<'a> = Option<&'a Entry<T>>;

    fn borrows() -> Vec<Borrow> {
        <&mut T as IsQuery>::borrows()
    }

    fn lock_columns<'a>(arch: &'a Archetype) -> Self::LockedColumns<'a> {
        <&T as IsQuery>::lock_columns(arch)
    }

    fn extend_locked_columns<'a, 'b>(
        _: &'b mut Self::LockedColumns<'a>,
        _: Self::ExtensionColumns,
        _: Option<(&mut Vec<usize>, &mut usize)>,
    ) {
        panic!("cannot extend Maybe columns");
    }

    fn iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        lock.as_mut().map_or_else(
            || {
                itertools::Either::Left(std::iter::repeat_with(
                    (|| None) as fn() -> Option<&'a Entry<T>>,
                ))
            },
            |data| {
                itertools::Either::Right(
                    data.downcast_ref::<Entry<T>>()
                        .expect("can't downcast")
                        .into_iter()
                        .map((|e| Some(e)) as fn(&Entry<T>) -> Option<&Entry<T>>),
                )
            },
        )
    }

    fn iter_one<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b> {
        lock.as_mut().map_or_else(
            || {
                itertools::Either::Left(std::iter::repeat_with(
                    (|| None) as fn() -> Option<&'b Entry<T>>,
                ))
            },
            |data| {
                itertools::Either::Right(
                    (&data
                        .downcast_ref::<Entry<T>>()
                        .expect("can't downcast")
                        .as_slice()[index..=index])
                        .into_iter()
                        .map((|e| Some(e)) as fn(&Entry<T>) -> Option<&Entry<T>>),
                )
            },
        )
    }

    fn par_iter_mut<'a, 'b>(
        len: usize,
        lock: &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
        lock.as_mut().map_or_else(
            || {
                rayon::iter::Either::Left(
                    rayon::iter::repeatn((), len)
                        .map((|()| None) as fn(()) -> Option<&'a Entry<T>>),
                )
            },
            |data| {
                rayon::iter::Either::Right(
                    data.downcast_ref::<Entry<T>>()
                        .expect("can't downcast")
                        .as_slice()
                        .into_par_iter()
                        .map((|e| Some(e)) as fn(&Entry<T>) -> Option<&Entry<T>>),
                )
            },
        )
    }
}

impl<'s, T: Send + Sync + 'static> IsQuery for Maybe<&'s mut T> {
    type LockedColumns<'a> = Option<RwLockWriteGuard<'a, AnyVec<dyn Send + Sync + 'static>>>;
    type ExtensionColumns = ();
    type QueryResult<'a> = itertools::Either<
        std::iter::RepeatWith<fn() -> Option<&'a mut Entry<T>>>,
        std::iter::Map<
            std::slice::IterMut<'a, Entry<T>>,
            for<'r> fn(&'r mut Entry<T>) -> Option<&'r mut Entry<T>>,
        >,
    >;
    type ParQueryResult<'a> = rayon::iter::Either<
        rayon::iter::Map<rayon::iter::RepeatN<()>, fn(()) -> Option<&'a mut Entry<T>>>,
        rayon::iter::Map<
            rayon::slice::IterMut<'a, Entry<T>>,
            for<'r> fn(&'r mut Entry<T>) -> Option<&'r mut Entry<T>>,
        >,
    >;
    type QueryRow<'a> = Option<&'a mut Entry<T>>;

    fn borrows() -> Vec<Borrow> {
        <&mut T as IsQuery>::borrows()
    }

    fn lock_columns<'a>(arch: &'a Archetype) -> Self::LockedColumns<'a> {
        <&mut T as IsQuery>::lock_columns(arch)
    }

    fn extend_locked_columns<'a, 'b>(
        _: &'b mut Self::LockedColumns<'a>,
        _: Self::ExtensionColumns,
        _: Option<(&mut Vec<usize>, &mut usize)>,
    ) {
        panic!("cannot extend Maybe columns");
    }

    fn iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        lock.as_mut().map_or_else(
            || {
                itertools::Either::Left(std::iter::repeat_with(
                    (|| None) as fn() -> Option<&'a mut Entry<T>>,
                ))
            },
            |data| {
                itertools::Either::Right(
                    data.downcast_mut::<Entry<T>>()
                        .expect("can't downcast")
                        .into_iter()
                        .map((|e| Some(e)) as fn(&mut Entry<T>) -> Option<&mut Entry<T>>),
                )
            },
        )
    }

    fn iter_one<'a, 'b>(
        lock: &'b mut Self::LockedColumns<'a>,
        index: usize,
    ) -> Self::QueryResult<'b> {
        lock.as_mut().map_or_else(
            || {
                itertools::Either::Left(std::iter::repeat_with(
                    (|| None) as fn() -> Option<&'b mut Entry<T>>,
                ))
            },
            |data| {
                itertools::Either::Right(
                    (&mut data
                        .downcast_mut::<Entry<T>>()
                        .expect("can't downcast")
                        .as_mut_slice()[index..=index])
                        .into_iter()
                        .map((|e| Some(e)) as fn(&mut Entry<T>) -> Option<&mut Entry<T>>),
                )
            },
        )
    }

    fn par_iter_mut<'a, 'b>(
        len: usize,
        lock: &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
        lock.as_mut().map_or_else(
            || {
                rayon::iter::Either::Left(
                    rayon::iter::repeatn((), len)
                        .map((|()| None) as fn(()) -> Option<&'a mut Entry<T>>),
                )
            },
            |data| {
                rayon::iter::Either::Right(
                    data.downcast_mut::<Entry<T>>()
                        .expect("can't downcast")
                        .as_mut_slice()
                        .into_par_iter()
                        .map((|e| Some(e)) as fn(&mut Entry<T>) -> Option<&mut Entry<T>>),
                )
            },
        )
    }
}

/// Query type used to denote the absence of the wrapped type.
pub struct Without<T>(PhantomData<T>);

impl<T: Send + Sync + 'static> IsQuery for Without<T> {
    type LockedColumns<'a> = bool;
    type ExtensionColumns = ();
    type QueryResult<'a> = itertools::Either<std::iter::Repeat<()>, std::vec::IntoIter<()>>;
    type ParQueryResult<'a> = rayon::iter::RepeatN<()>;
    type QueryRow<'a> = ();

    fn borrows() -> Vec<Borrow> {
        <&T as IsQuery>::borrows()
    }

    fn lock_columns<'a>(arch: &'a Archetype) -> Self::LockedColumns<'a> {
        arch.has_component::<T>()
    }

    fn extend_locked_columns<'a, 'b>(
        _: &'b mut Self::LockedColumns<'a>,
        _: Self::ExtensionColumns,
        _: Option<(&mut Vec<usize>, &mut usize)>,
    ) {
        panic!("cannot extend Without columns");
    }

    fn iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        if *lock {
            itertools::Either::Right(vec![].into_iter())
        } else {
            itertools::Either::Left(std::iter::repeat(()))
        }
    }

    fn iter_one<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>, _: usize) -> Self::QueryResult<'b> {
        if *lock {
            itertools::Either::Right(vec![].into_iter())
        } else {
            itertools::Either::Left(std::iter::repeat(()))
        }
    }

    fn par_iter_mut<'a, 'b>(
        len: usize,
        lock: &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
        if *lock {
            rayon::iter::repeatn((), 0)
        } else {
            rayon::iter::repeatn((), len)
        }
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
        output_ids: Option<(&mut Vec<usize>, &mut usize)>,
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

    fn par_iter_mut<'a, 'b>(
        len: usize,
        lock: &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
        A::par_iter_mut(len, lock)
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
        output_ids: Option<(&mut Vec<usize>, &mut usize)>,
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
        len: usize,
        (col_a, col_b): &'b mut Self::LockedColumns<'a>,
    ) -> Self::ParQueryResult<'b> {
        A::par_iter_mut(len, col_a).zip(B::par_iter_mut(len, col_b))
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

impl Components {
    /// Append to the existing set of archetypes in bulk.
    ///
    /// This assumes the entries being inserted have unique ids and don't
    /// already exist in the set.
    ///
    /// ```rust
    /// use apecs::*;
    /// let mut components = Components::default();
    /// let a = Box::new((0..10_000).map(|id| Entry::new(id, id as u32)));
    /// let b = Box::new((0..10_000).map(|id| Entry::new(id, id as f32)));
    /// let c = Box::new((0..10_000).map(|id| Entry::new(id, format!("string{}", id))));
    /// let d = Box::new((0..10_000).map(|id| Entry::new(id, id % 2 == 0)));
    /// components.extend::<(u32, f32, String, bool)>((a, b, c, d));
    /// assert_eq!(10_000, components.len());
    /// ```
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
        let mut max_id: usize = 0;
        {
            let mut lock = <B::MutBundle as IsQuery>::lock_columns(arch);
            <B::MutBundle as IsQuery>::extend_locked_columns(
                &mut lock,
                extension,
                Some((&mut index_lookup, &mut max_id)),
            );
        }
        arch.index_lookup = index_lookup.clone();
        drop(arch);

        if max_id >= self.entity_lookup.len() {
            self.entity_lookup.resize_with(max_id + 1, Default::default);
        }
        index_lookup.iter().enumerate().for_each(|(i, id)| {
            self.entity_lookup[*id] = Some((archetype_index, i));
        });
    }

    /// Prepare a query.
    /// ```rust
    /// use apecs::*;
    ///
    /// let mut components = Components::default();
    /// components.insert_bundle(0, ("zero", 0.0f32, 0u32));
    /// components.insert_bundle(1, ("zero", 0.0f32, 0u32));
    /// components.insert_bundle(2, ("zero", 0.0f32, 0u32));
    ///
    /// for (f, u) in components.query::<(&f32, &u32)>().iter_mut() {
    ///     assert_eq!(**f, **u as f32);
    /// }
    /// ```
    pub fn query<Q: IsQuery + 'static>(&mut self) -> QueryGuard<'_, Q> {
        QueryGuard(
            self.archetypes.iter().map(|a| Q::lock_columns(a)).collect(),
            self,
        )
    }
}

/// Iterator returned by [`QueryGuard::iter_mut`].
pub type QueryIter<'a, 'b, Q> = std::iter::FlatMap<
    std::slice::IterMut<'b, <Q as IsQuery>::LockedColumns<'a>>,
    <Q as IsQuery>::QueryResult<'b>,
    for<'r> fn(&'r mut <Q as IsQuery>::LockedColumns<'a>) -> <Q as IsQuery>::QueryResult<'r>,
>;

/// A prepared and active query.
///
/// Iterate over queried items using [`QueryGuard::iter_mut`].
pub struct QueryGuard<'a, Q: IsQuery + ?Sized>(Vec<Q::LockedColumns<'a>>, &'a Components);

impl<'a, Q> QueryGuard<'a, Q>
where
    Q: IsQuery + ?Sized,
{
    /// Iterate over matched component bundles.
    ///
    /// Each component is wrapped with [`Entry`], which provides access
    /// to the entity id and modification data.
    pub fn iter_mut(&mut self) -> QueryIter<'a, '_, Q> {
        self.0.iter_mut().flat_map(|cols| Q::iter_mut(cols))
    }

    /// Find the component bundle with the given entity id.
    ///
    /// This does not iterate. It locates the item by index.
    pub fn find_one(&mut self, entity_id: usize) -> Option<Q::QueryRow<'_>> {
        self.1
            .entity_lookup
            .get(entity_id)
            .copied()
            .and_then(|may_indices| {
                let (archetype_index, component_index) = may_indices?;
                let mut vs =
                    Q::iter_one(&mut self.0[archetype_index], component_index).collect::<Vec<_>>();
                vs.pop()
            })
    }

    /// Iterate over matched component bundles, in parallel.
    ///
    /// Each component is wrapped with [`Entry`], which provides access
    /// to the entity id and modification data.
    pub fn par_iter_mut(
        &mut self,
    ) -> rayon::iter::Flatten<rayon::vec::IntoIter<<Q as IsQuery>::ParQueryResult<'_>>> {
        let size_hints = self.1.archetypes.iter().map(|arch| arch.index_lookup.len());
        size_hints
            .zip(self.0.iter_mut())
            .map(|(len, cols)| Q::par_iter_mut(len, cols))
            .collect::<Vec<_>>()
            .into_par_iter()
            .flatten()
    }
}

/// Queries are used to iterate over matching bundles of components.
///
/// `Query`s are written as a tuple of component column types, where each
/// position in the tuple denotes a column value in each item of the resulting
/// query iterator. Each item column type will be wrapped in [`Entry`] in the
/// resulting query iterator.
///
/// For example: `Query<(&mut f32, &String)>` will result in an iterator of
/// items with the type `(&mut Entry<f32>, &Entry<String>)`.
///
/// ## Creating queries
/// Some functions query immidiately and return [`QueryGuard`]:
/// * [`World::query`](crate::World::query)
/// * [`Components::query`]
/// * [`Entity::visit`](crate::Entity::visit)
///
/// With these functions you pass component column types as a type parameter:
/// ```
/// # use apecs::*;
/// # let mut world = World::default();
/// let mut query = world.query::<(&f32, &String)>();
/// for (f32, string) in query.iter_mut() {
///     //...
/// }
/// ```
///
/// ### System data queries
/// `Query` implements [`CanFetch`], which allows you to use queries
/// as fields inside system data structs:
///
/// ```
/// # use apecs::*;
/// #[derive(CanFetch)]
/// struct MySystemData {
///     counter: Write<u32>,
///     // we use &'static to avoid introducing a lifetime
///     q_f32_and_string: Query<(&'static f32, &'static String)>,
/// }
/// ```
///
/// Which means queries may be [`fetch`](crate::World::fetch)ed from the world,
/// [`visit`](crate::Facade::visit)ed from a facade or used as the input to a
/// system:
/// ```
/// # use apecs::*;
/// #
/// #[derive(CanFetch)]
/// struct MySystemData {
///     tracker: Write<u64>,
///     // we can use Mut and Ref which are aliases for &'static mut and &'static
///     q_f32_and_string: Query<(Mut<f32>, Ref<String>)>,
/// }
/// let mut world = World::default();
/// world
///     .with_system("query_example", |mut data: MySystemData| {
///         for (f32, string) in data.q_f32_and_string.query().iter_mut() {
///             if f32.was_modified_since(*data.tracker) {
///                 **f32 += 1.0;
///                 println!("set entity {} = {}", f32.id(), **f32);
///             }
///         }
///         *data.tracker = apecs::current_iteration();
///         ok()
///     })
///     .unwrap();
/// world.tick();
/// ```
///
/// ## Iterating queries
/// Below we use mutable and immutable reference types in our query to include
/// either a mutable or immutable reference to those components:
/// ```
/// use apecs::*;
///
/// struct Position(pub f32);
/// struct Velocity(pub f32);
/// struct Acceleration(pub f32);
///
/// let mut world = World::default();
/// world
///     .with_system("create", |mut entities: Write<Entities>| {
///         for i in 0..100 {
///             entities.create().insert_bundle((
///                 Position(0.0),
///                 Velocity(0.0),
///                 Acceleration(i as f32),
///             ));
///         }
///         end()
///     })
///     .unwrap()
///     .with_system(
///         "accelerate",
///         |q_accelerate: Query<(&mut Velocity, &Acceleration)>| {
///             for (v, a) in q_accelerate.query().iter_mut() {
///                 v.0 += a.0;
///             }
///             ok()
///         },
///     )
///     .unwrap()
///     .with_system("move", |q_move: Query<(&mut Position, &Velocity)>| {
///         for (p, v) in q_move.query().iter_mut() {
///             p.0 += v.0;
///         }
///         ok()
///     })
///     .unwrap();
/// ```
pub struct Query<T>(
    Box<dyn Deref<Target = Components> + Send + Sync + 'static>,
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
        bs.extend(Read::<Components>::borrows());
        bs
    }

    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self> {
        let all = Read::<Components>::construct(loan_mngr)?;
        Ok(Query(Box::new(all), PhantomData))
    }
}

impl<Q> Query<Q>
where
    Q: IsQuery + ?Sized,
{
    /// Prepare the query, locking the columns needed to iterate over matching
    /// components.
    pub fn query(&self) -> QueryGuard<'_, Q> {
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

/// Alias for `&'static T` for folks with wrist pain.
pub type Ref<T> = &'static T;
/// Alias for `&'static mut T` for folks with wrist pain.
pub type Mut<T> = &'static mut T;
/// Alias for `Maybe<&'static T>` for folks with wrist pain.
pub type MaybeRef<T> = Maybe<&'static T>;
/// Alias for `Maybe<&'static mut T>` for folks with wrist pain.
pub type MaybeMut<T> = Maybe<&'static mut T>;

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

        let mut all = Components::default();
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
        let mut store = Components::default();
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
        let mut store = Components::default();
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
        let mut store = Components::default();
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

    #[test]
    fn can_query_maybe() {
        let mut components = Components::default();
        components.insert_bundle(0, (0.0f32, "zero".to_string()));
        components.insert_bundle(1, (1.0f32, "one".to_string(), false));
        components.insert_bundle(2, (2.0f32, "two".to_string()));

        let cols = components
            .query::<(&f32, Maybe<&bool>, &String)>()
            .iter_mut()
            .map(|(f, mb, s)| (**f, mb.map(|b| **b), s.value().clone()))
            .collect::<Vec<_>>();
        // these appear in order of archetypes added
        assert_eq!(
            vec![
                (0.0, None, "zero".to_string()),
                (2.0, None, "two".to_string()),
                (1.0, Some(false), "one".to_string()),
            ],
            cols
        );

        let par_cols = components
            .query::<(&f32, Maybe<&bool>, &String)>()
            .par_iter_mut()
            .map(|(f, mb, s)| (**f, mb.map(|b| **b), s.value().clone()))
            .collect::<Vec<_>>();
        assert_eq!(cols, par_cols);
    }

    #[test]
    fn can_query_without() {
        let mut components = Components::default();
        components.insert_bundle(0, (0.0f32, "zero".to_string()));
        components.insert_bundle(1, (1.0f32, "one".to_string(), false));
        components.insert_bundle(2, (2.0f32, "two".to_string()));

        let cols = components
            .query::<(&f32, Without<bool>, &String)>()
            .iter_mut()
            .map(|(f, (), s)| (**f, (), s.value().clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            vec![(0.0, (), "zero".to_string()), (2.0, (), "two".to_string()),],
            cols
        );

        let par_cols = components
            .query::<(&f32, Without<bool>, &String)>()
            .par_iter_mut()
            .map(|(f, (), s)| (**f, (), s.value().clone()))
            .collect::<Vec<_>>();
        assert_eq!(cols, par_cols);
    }
}
