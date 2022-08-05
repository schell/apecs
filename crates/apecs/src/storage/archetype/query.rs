//! Archetype queries.
use std::{
    any::TypeId,
    marker::PhantomData,
    sync::{RwLockReadGuard, RwLockWriteGuard},
};

use crate::{
    resource_manager::LoanManager,
    schedule::Borrow,
    storage::{
        archetype::{AllArchetypes, Archetype},
        EntityInfo, Mut, Ref,
    },
    CanFetch, Read, ResourceId,
};
use any_vec::{traits::*, AnyVec};

/// An iterator over one archetype column.
pub struct ColumnIter<'a, T>(std::slice::Iter<'a, T>, std::slice::Iter<'a, EntityInfo>);

impl<'a, T: 'static> Iterator for ColumnIter<'a, T> {
    type Item = Ref<'a, T>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let d = self.0.next()?;
        let i = self.1.next()?;
        Some(Ref(i, d))
    }
}

impl<T: 'static> Default for ColumnIter<'static, T> {
    fn default() -> Self {
        Self((&[]).into_iter(), (&[]).into_iter())
    }
}

pub struct ColumnIterMut<'a, T>(
    std::slice::IterMut<'a, T>,
    std::slice::IterMut<'a, EntityInfo>,
);

impl<'a, T: 'static> Iterator for ColumnIterMut<'a, T> {
    type Item = Mut<'a, T>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let d = self.0.next()?;
        let i = self.1.next()?;
        Some(Mut(i, d))
    }
}

impl<T: 'static> Default for ColumnIterMut<'static, T> {
    fn default() -> Self {
        Self((&mut []).into_iter(), (&mut []).into_iter())
    }
}

/// A placeholder type for borrowing an entire column of components from
/// the world.
pub struct ComponentColumn<T>(PhantomData<T>);

pub trait IsQuery {
    type LockedColumns<'a>;
    type QueryResult<'a>: Iterator<Item = Self::QueryRow<'a>>;
    type QueryRow<'a>;

    fn borrows() -> Vec<Borrow>;
    /// Find and acquire a "lock" on the columns for reading or writing.
    fn lock_columns<'a>(arch: &'a Archetype) -> Self::LockedColumns<'a>;

    /// Create an iterator over the rows of the given columns.
    fn iter_mut<'a, 'b>(lock: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b>;
}

impl<'s, T: Send + Sync + 'static> IsQuery for &'s T {
    type LockedColumns<'a> = Option<(
        RwLockReadGuard<'a, AnyVec<dyn Send + Sync + 'static>>,
        RwLockReadGuard<'a, Vec<EntityInfo>>,
    )>;
    type QueryResult<'a> = ColumnIter<'a, T>;
    type QueryRow<'a> = Ref<'a, T>;

    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<ComponentColumn<T>>(),
            is_exclusive: false,
        }]
    }

    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        log::trace!("reading {}", std::any::type_name::<T>());
        let ty = TypeId::of::<T>();
        let i = arch.index_of(&ty)?;
        let data = arch.data[i].read().expect("can't read");
        let info = arch.entity_info[i].read().expect("can't read");
        log::trace!("  got read lock on {}", std::any::type_name::<T>());
        Some((data, info))
    }

    fn iter_mut<'a, 'b>(locked: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        log::trace!("creating iter &{}", std::any::type_name::<T>());
        locked
            .as_ref()
            .map(|(data, infos)| {
                log::trace!("got column");
                ColumnIter(
                    data.downcast_ref::<T>()
                        .expect("can't downcast")
                        .into_iter(),
                    infos.iter(),
                )
            })
            .unwrap_or_else(|| {
                log::trace!("empty column");
                ColumnIter::default()
            })
    }
}

impl<'s, T: Send + Sync + 'static> IsQuery for &'s mut T {
    type LockedColumns<'a> = Option<(
        RwLockWriteGuard<'a, AnyVec<dyn Send + Sync + 'static>>,
        RwLockWriteGuard<'a, Vec<EntityInfo>>,
    )>;
    type QueryResult<'a> = ColumnIterMut<'a, T>;
    type QueryRow<'a> = Mut<'a, T>;

    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<ComponentColumn<T>>(),
            is_exclusive: true,
        }]
    }

    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        log::trace!("locked &mut {}", std::any::type_name::<T>());
        let ty = TypeId::of::<T>();
        let i = arch.index_of(&ty)?;
        let data = arch.data[i].write().expect("can't read");
        let info = arch.entity_info[i].write().expect("can't read");
        Some((data, info))
    }

    fn iter_mut<'a, 'b>(locked: &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        log::trace!("creating iter &{}", std::any::type_name::<T>());
        locked
            .as_mut()
            .map(|(data, infos)| {
                log::trace!("got column");
                ColumnIterMut(
                    data.downcast_mut::<T>()
                        .expect("can't downcast")
                        .into_iter(),
                    infos.iter_mut(),
                )
            })
            .unwrap_or_else(|| ColumnIterMut::default())
    }
}

impl<A, B> IsQuery for (A, B)
where
    A: IsQuery,
    B: IsQuery,
{
    type LockedColumns<'a> = (A::LockedColumns<'a>, B::LockedColumns<'a>);
    type QueryResult<'a> = std::iter::Zip<A::QueryResult<'a>, B::QueryResult<'a>>;
    type QueryRow<'a> = (A::QueryRow<'a>, B::QueryRow<'a>);

    fn borrows() -> Vec<Borrow> {
        let mut bs = A::borrows();
        bs.extend(B::borrows());
        bs
    }

    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        let a = A::lock_columns(arch);
        let b = B::lock_columns(arch);
        (a, b)
    }

    fn iter_mut<'a, 'b>((col_a, col_b): &'b mut Self::LockedColumns<'a>) -> Self::QueryResult<'b> {
        A::iter_mut(col_a).zip(B::iter_mut(col_b))
    }
}

impl<A, B, C> IsQuery for (A, B, C)
where
    A: IsQuery,
    B: IsQuery,
    C: IsQuery,
{
    type LockedColumns<'a> = (
        A::LockedColumns<'a>,
        B::LockedColumns<'a>,
        C::LockedColumns<'a>,
    );
    type QueryResult<'a> = std::iter::Map<
        std::iter::Zip<std::iter::Zip<A::QueryResult<'a>, B::QueryResult<'a>>, C::QueryResult<'a>>,
        fn(
            ((A::QueryRow<'a>, B::QueryRow<'a>), C::QueryRow<'a>),
        ) -> (A::QueryRow<'a>, B::QueryRow<'a>, C::QueryRow<'a>),
    >;
    type QueryRow<'a> = (A::QueryRow<'a>, B::QueryRow<'a>, C::QueryRow<'a>);

    fn borrows() -> Vec<Borrow> {
        let mut bs = A::borrows();
        bs.extend(B::borrows());
        bs.extend(C::borrows());
        bs
    }

    fn lock_columns<'t>(arch: &'t Archetype) -> Self::LockedColumns<'t> {
        let a = A::lock_columns(arch);
        let b = B::lock_columns(arch);
        let c = C::lock_columns(arch);
        (a, b, c)
    }

    fn iter_mut<'a, 'b>(
        (col_a, col_b, col_c): &'b mut Self::LockedColumns<'a>,
    ) -> Self::QueryResult<'b> {
        A::iter_mut(col_a)
            .zip(B::iter_mut(col_b))
            .zip(C::iter_mut(col_c))
            .map(|((a, b), c)| (a, b, c))
    }
}

pub struct QueryGuard<'a, T>(Vec<T::LockedColumns<'a>>)
where
    T: IsQuery + ?Sized;

impl<'a, T: IsQuery> QueryGuard<'a, T> {
    pub fn for_each(&mut self, mut f: impl FnMut(T::QueryRow<'_>)) {
        for mut cols in self.0.iter_mut() {
            for row in T::iter_mut(&mut cols) {
                f(row);
            }
        }
    }
}

/// A query that has been prepared over all archetypes.
pub struct Query<T>(Read<AllArchetypes>, PhantomData<T>)
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
        Ok(Query(all, PhantomData))
    }
}

impl<T: IsQuery> Query<T> {
    pub fn prepare(&self) -> QueryGuard<'_, T> {
        let locks = self
            .0
            .archetypes
            .iter()
            .map(|a| T::lock_columns(a))
            .collect();
        QueryGuard(locks)
    }
}

impl Archetype {
    #[inline]
    pub fn for_each<Q>(&self, mut f: impl FnMut(Q::QueryRow<'_>))
    where
        Q: IsQuery,
    {
        let mut cols = Q::lock_columns(self);
        for row in Q::iter_mut(&mut cols) {
            f(row);
        }
    }
}

impl AllArchetypes {
    #[inline]
    pub fn for_each<Q>(&self, mut f: impl FnMut(Q::QueryRow<'_>))
    where
        Q: IsQuery,
    {
        for arch in self.archetypes.iter() {
            arch.for_each::<Q>(&mut f);
        }
    }
}
// impl<'a, T> From<&'a mut AllArchetypes> for Query<T>
// where
//    T: IsQuery + Send + Sync + ?Sized,
//{
//    fn from(store: &'a mut AllArchetypes) -> Self {
//        let columns = T::pop_columns(store);
//        Query(columns)
//    }
//}
// impl<T> Query<T>
// where
//    T: IsQuery + ?Sized,
//{
//    pub fn run(&mut self) -> T::ColumnsIter<'_> {
//        T::columns_iter(&mut self.0)
//    }
//}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn can_query_archetype() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut arch = Archetype::new::<(f32,)>().unwrap();
        arch.insert(0, (0.0f32,)).unwrap();
        arch.insert(1, (1.0f32,)).unwrap();
        arch.insert(2, (2.0f32,)).unwrap();
        arch.insert(3, (3.0f32,)).unwrap();

        let mut lock = <&f32>::lock_columns(&arch);
        let iter = <&f32>::iter_mut(&mut lock);
        let result: f32 = iter.map(|f| *f).sum();
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
            sum += *f;
        });
        assert_eq!(6.0, sum);
    }
}
