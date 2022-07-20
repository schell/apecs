//! Entity components stored together in contiguous arrays.
use std::{any::TypeId, marker::PhantomData, ops::DerefMut};

use any_vec::{any_value::AnyValueMut, mem::Heap, traits::*, AnyVec, AnyVecMut, AnyVecRef};
use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

mod bundle;
pub use bundle::*;
use tuple_list::TupleList;

use crate::{mpsc, CanFetch, ResourceId, schedule::Borrow};

/// A collection of entities having the same component types
#[derive(Debug, Default)]
pub struct Archetype {
    pub(crate) types: FxHashSet<TypeId>,
    pub(crate) entities: Vec<usize>,
    // map of entity_id to storage index
    pub(crate) entity_lookup: FxHashMap<usize, usize>,
    pub(crate) data: FxHashMap<TypeId, AnyVec<dyn Sync + Send>>,
}

impl TryFrom<()> for Archetype {
    type Error = anyhow::Error;

    fn try_from(_: ()) -> anyhow::Result<Self> {
        Ok(Archetype::default())
    }
}

impl<Head, Tail> TryFrom<(Head, Tail)> for Archetype
where
    Head: Send + Sync + 'static,
    Tail: TupleList,
    Archetype: From<Tail>,
{
    type Error = anyhow::Error;

    fn try_from((head, tail): (Head, Tail)) -> anyhow::Result<Self> {
        let mut archetype = Archetype::try_from(tail)?;
        let ty = TypeId::of::<Head>();
        let type_is_unique_in_archetype = archetype.types.insert(ty.clone());
        anyhow::ensure!(
            type_is_unique_in_archetype,
            "type is already in the archetype"
        );
        let mut anyvec: AnyVec<dyn Send + Sync> = AnyVec::new::<Head>();
        {
            let mut vec = anyvec
                .downcast_mut::<Head>()
                .context("could not downcast")?;
            vec.push(head);
        }
        let _ = archetype.data.insert(ty, anyvec);
        Ok(archetype)
    }
}

/// ## NOTE: The resulting Archetype will not have any entities associated
/// with the data populated by the given AnyBundle.
impl TryFrom<AnyBundle> for Archetype {
    type Error = anyhow::Error;

    fn try_from(bundle: AnyBundle) -> Result<Self, Self::Error> {
        let mut types = bundle.0.keys().map(Clone::clone).collect::<Vec<_>>();
        types.sort();
        bundle::ensure_type_info(&types)?;
        let types = FxHashSet::from_iter(types);
        Ok(Archetype {
            types,
            data: bundle.0,
            ..Default::default()
        })
    }
}

impl Archetype {
    pub fn new<B>() -> anyhow::Result<Self>
    where
        B: IsBundleTypeInfo + 'static,
        B::TupleList: EmptyBundle,
    {
        let bundle = B::TupleList::empty_bundle();
        Archetype::try_from(bundle)
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Whether this archetype contains the entity
    pub fn contains_entity(&self, entity_id: &usize) -> bool {
        self.entity_lookup.contains_key(entity_id)
    }

    /// Whether this archetype contains `T` components
    pub fn has<T: 'static>(&self) -> bool {
        self.has_type(&TypeId::of::<T>())
    }

    /// Whether this archetype contains components with the type identified by
    /// `id`
    pub fn has_type(&self, ty: &TypeId) -> bool {
        self.types.contains(ty)
    }

    /// Whether this archetype's types match the given bundle type exactly.
    pub fn matches_bundle<B: IsBundleTypeInfo + 'static>(&self) -> anyhow::Result<bool> {
        let types = FxHashSet::from_iter(bundle::type_info::<B>()?.into_iter());
        Ok(self.types == types)
    }

    /// Whether this archetype's types contain all of the bundle's component
    /// types
    pub fn contains_bundle_components(&self, types: &FxHashSet<TypeId>) -> bool {
        self.types.is_superset(types)
    }

    pub fn get_column<T: 'static>(&self) -> anyhow::Result<AnyVecRef<'_, T, Heap>> {
        let ty = TypeId::of::<T>();
        let storage = self
            .data
            .get(&ty)
            .with_context(|| format!("no such column {}", std::any::type_name::<T>()))?;
        storage
            .downcast_ref::<T>()
            .with_context(|| format!("could not downcast to {}", std::any::type_name::<T>()))
    }

    pub fn get_column_mut<T: 'static>(&mut self) -> anyhow::Result<AnyVecMut<'_, T, Heap>> {
        let ty = TypeId::of::<T>();
        let storage = self
            .data
            .get_mut(&ty)
            .with_context(|| format!("no such column {}", std::any::type_name::<T>()))?;
        storage
            .downcast_mut::<T>()
            .with_context(|| format!("could not downcast to {}", std::any::type_name::<T>()))
    }

    pub fn take_column(&mut self, ty: &TypeId) -> Option<AnyVec<dyn Send + Sync>> {
        self.data.remove(ty)
    }

    pub fn insert<B>(&mut self, entity_id: usize, bundle: B) -> anyhow::Result<Option<B>>
    where
        B: Tuple + 'static,
        AnyBundle: From<B::TupleList>,
        <B as Tuple>::TupleList: TryFromAnyBundle,
    {
        Ok(
            if let Some(prev) = self.insert_any(entity_id, AnyBundle::from_tuple(bundle))? {
                let bundle = prev.try_into_tuple()?;
                Some(bundle)
            } else {
                None
            },
        )
    }

    pub fn insert_any(
        &mut self,
        entity_id: usize,
        mut bundle: AnyBundle,
    ) -> anyhow::Result<Option<AnyBundle>> {
        Ok(if let Some(index) = self.entity_lookup.get(&entity_id) {
            for (ty, component_vec) in bundle.0.iter_mut() {
                let column = self.data.get_mut(ty).context("missing column")?;
                let mut element_a = column.at_mut(*index);
                let mut element_b = component_vec.at_mut(0);
                element_a.swap(element_b.deref_mut());
            }
            Some(bundle)
        } else {
            for (ty, mut component_vec) in bundle.0.into_iter() {
                let column = self.data.get_mut(&ty).context("missing column")?;
                column.push(component_vec.pop().unwrap());
            }
            assert!(self
                .entity_lookup
                .insert(entity_id, self.entities.len())
                .is_none());
            self.entities.push(entity_id);
            None
        })
    }

    /// Remove the entity and return its bundle, if any.
    pub fn remove<B>(&mut self, entity_id: usize) -> anyhow::Result<Option<B>>
    where
        B: Tuple + 'static,
        <B as Tuple>::TupleList: TryFromAnyBundle,
    {
        Ok(if let Some(prev) = self.remove_any(entity_id)? {
            Some(prev.try_into_tuple()?)
        } else {
            None
        })
    }

    /// Remove the entity without knowledge of the bundle type and return its
    /// type-erased representation
    pub fn remove_any(&mut self, entity_id: usize) -> anyhow::Result<Option<AnyBundle>> {
        if let Some(index) = self.entity_lookup.remove(&entity_id) {
            let should_reset_index = index != self.entities.len() - 1;
            assert_eq!(self.entities.swap_remove(index), entity_id);
            if should_reset_index {
                // the index of the last entity was swapped for that of the removed index, so
                // we need to update the lookup
                self.entity_lookup.insert(self.entities[index], index);
            }
            let anybundle =
                self.data
                    .iter_mut()
                    .fold(AnyBundle::default(), |mut bundle, (_ty, store)| {
                        let mut temp_store = store.clone_empty();
                        debug_assert!(
                            index < store.len(),
                            "store len {} does not contain index {}",
                            store.len(),
                            index
                        );
                        let value = store.swap_remove(index);
                        temp_store.push(value);
                        bundle.0.insert(store.element_typeid(), temp_store);
                        bundle
                    });
            Ok(Some(anybundle))
        } else {
            Ok(None)
        }
    }

    /// Get a single component reference.
    pub fn get<T: 'static>(&self, entity_id: usize) -> anyhow::Result<Option<&T>> {
        if let Some(index) = self.entity_lookup.get(&entity_id) {
            let storage = self.get_column::<T>()?;
            Ok(storage.get(*index))
        } else {
            Ok(None)
        }
    }

    /// Get a mutable component reference.
    pub fn get_mut<T: 'static>(&mut self, entity_id: usize) -> anyhow::Result<Option<&mut T>> {
        if let Some(index) = self.entity_lookup.get(&entity_id).copied() {
            let mut storage = self.get_column_mut::<T>()?;
            Ok(storage.get_mut(index))
        } else {
            Ok(None)
        }
    }
}

struct QueryReturn(
    TypeId,
    Vec<ArchetypeColumnInfo>,
    Vec<AnyVec<dyn Send + Sync>>,
);

#[derive(Debug)]
pub struct AllArchetypes {
    archetypes: Vec<Archetype>,
    query_return_chan: (mpsc::Sender<QueryReturn>, mpsc::Receiver<QueryReturn>),
}

impl Default for AllArchetypes {
    fn default() -> Self {
        Self {
            archetypes: Default::default(),
            query_return_chan: mpsc::unbounded(),
        }
    }
}

impl AllArchetypes {
    fn unify_resources(&mut self) {
        while let Some(QueryReturn(ty, infos, data)) = self.query_return_chan.1.try_recv().ok() {
            for (info, datum) in infos.into_iter().zip(data.into_iter()) {
                let archetype = &mut self.archetypes[info.archetype_index];
                archetype.entities = info.entity_info;
                let prev = archetype.data.insert(ty, datum);
                debug_assert!(
                    prev.is_none(),
                    "inserted query return data into archetype that already had data"
                );
            }
        }
    }

    pub fn get_column<T: 'static>(&mut self) -> Option<QueryColumn<T>> {
        self.unify_resources();
        let ty = TypeId::of::<T>();
        let mut column = QueryColumn::<T>::new(self.query_return_chan.0.clone()); //
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if let Some(col) = arch.take_column(&ty) {
                column.info.push(ArchetypeColumnInfo {
                    archetype_index: i,
                    entity_info: std::mem::take(&mut arch.entities),
                });
                column.data.push(col);
            }
        }
        if column.data.is_empty() {
            None
        } else {
            Some(column)
        }
    }

    pub fn get_column_mut<T: 'static>(&mut self) -> Option<QueryColumnMut<T>> {
        Some(QueryColumnMut(self.get_column()?))
    }

    /// Inserts the bundle components for the given entity.
    ///
    /// ## Panics
    /// * if the bundle's types are not unique
    pub fn insert_bundle<B>(&mut self, entity_id: usize, bundle: B) -> Option<B>
    where
        B: IsBundleTypeInfo + 'static,
        B::TupleList: TryFromAnyBundle,
        AnyBundle: From<B::TupleList>,
    {
        let mut bundle_types: FxHashSet<TypeId> = bundle::type_info::<B>().unwrap();
        let mut any_bundle = AnyBundle::from_tuple(bundle);
        let mut prev_bundle: Option<AnyBundle> = None;

        // first find if the entity already exists
        for arch in self.archetypes.iter_mut() {
            if arch.contains_entity(&entity_id) {
                if arch.contains_bundle_components(&bundle_types) {
                    // we found a perfect match, simply store the bundle,
                    // returning the previous components
                    let prev = arch.insert_any(entity_id, any_bundle).unwrap().unwrap();
                    return Some(prev.try_into_tuple().unwrap());
                }
                // otherwise we remove the bundle from the archetype, adding the
                // non-overlapping key+values to our bundle
                let mut old_bundle = arch.remove_any(entity_id).unwrap().unwrap();
                prev_bundle = old_bundle.union(any_bundle);
                any_bundle = old_bundle;
                bundle_types = any_bundle.type_info();
                break;
            }
        }

        // now search for an archetype to store the bundle
        for arch in self.archetypes.iter_mut() {
            if arch.contains_bundle_components(&bundle_types) {
                // we found an archetype that will store our bundle, so store it
                let no_previous_bundle = arch.insert_any(entity_id, any_bundle).unwrap().is_none();
                assert!(
                    no_previous_bundle,
                    "the archetype had a bundle at the entity"
                );
                return None;
            }
        }

        // we couldn't find any existing archetypes that can hold the bundle, so make a
        // new one
        let mut new_arch = Archetype::try_from(any_bundle).unwrap();
        new_arch.entities = vec![entity_id];
        new_arch.entity_lookup = FxHashMap::from_iter([(entity_id, 0)]);
        self.archetypes.push(new_arch);
        prev_bundle.and_then(|b| b.try_into_tuple().ok())
    }

    pub fn insert<T: Send + Sync + 'static>(
        &mut self,
        entity_id: usize,
        component: T,
    ) -> Option<T> {
        self.insert_bundle(entity_id, (component,)).map(|(t,)| t)
    }

    pub fn get<T: 'static>(&self, entity_id: &usize) -> Option<&T> {
        for arch in self.archetypes.iter() {
            if arch.has::<T>() && arch.contains_entity(entity_id) {
                return arch.get::<T>(*entity_id).unwrap();
            }
        }
        None
    }

    pub fn get_mut<T: 'static>(&mut self, entity_id: &usize) -> Option<&mut T> {
        for arch in self.archetypes.iter_mut() {
            if arch.has::<T>() && arch.contains_entity(entity_id) {
                return arch.get_mut::<T>(*entity_id).unwrap();
            }
        }
        None
    }

    /// Remove all components of the given entity and return them in an untyped
    /// bundle.
    pub fn remove_any(&mut self, entity_id: usize) -> Option<AnyBundle> {
        for arch in self.archetypes.iter_mut() {
            if let Some(bundle) = arch.remove_any(entity_id).unwrap() {
                return Some(bundle);
            }
        }
        None
    }

    /// Remove all components of the given entity and return them as a typed
    /// bundle.
    pub fn remove<B>(&mut self, entity_id: usize) -> Option<B>
    where
        B: IsBundleTypeInfo + 'static,
        B::TupleList: TryFromAnyBundle,
        AnyBundle: From<B::TupleList>,
    {
        let bundle = self.remove_any(entity_id)?;
        Some(bundle.try_into_tuple().unwrap())
    }

    /// Remove a single component from the given entity
    pub fn remove_component<T: Send + Sync + 'static>(&mut self, entity_id: usize) -> Option<T> {
        fn only_remove<S: Send + Sync + 'static>(
            id: usize,
            all: &mut AllArchetypes,
        ) -> (Option<AnyBundle>, Option<S>) {
            for arch in all.archetypes.iter_mut() {
                if let Some(mut bundle) = arch.remove_any(id).unwrap() {
                    let ty = TypeId::of::<S>();
                    let prev = bundle.remove(ty).map(|b| {
                        let (s,): (S,) = b.try_into_tuple().unwrap();
                        s
                    });
                    return (Some(bundle), prev);
                }
            }
            (None, None)
        }
        let (bundle, prev) = only_remove(entity_id, self);

        let bundle = bundle?;
        let bundle_types = bundle.type_info();
        // now search for an archetype to store the bundle
        for arch in self.archetypes.iter_mut() {
            if arch.contains_bundle_components(&bundle_types) {
                // we found an archetype that will store our bundle, so store it
                let no_previous_bundle = arch.insert_any(entity_id, bundle).unwrap().is_none();
                assert!(
                    no_previous_bundle,
                    "the archetype had a bundle at the entity"
                );
                return prev;
            }
        }

        // we couldn't find any existing archetypes that can hold the bundle, so make a
        // new one
        let mut new_arch = Archetype::try_from(bundle).unwrap();
        new_arch.entities = vec![entity_id];
        new_arch.entity_lookup = FxHashMap::from_iter([(entity_id, 0)]);
        self.archetypes.push(new_arch);

        prev
    }

    /// Return the number of entities with archetypes.
    pub fn len(&self) -> usize {
        self.archetypes.iter().map(|a| a.len()).sum()
    }

    /// Return whether any entities with archetypes exist in storage.
    pub fn is_empty(&self) -> bool {
        self.archetypes.iter().all(|a| a.is_empty())
    }
}

struct ArchetypeColumnInfo {
    // This column's archetype index in the AllArchetypes struct
    archetype_index: usize,
    // Extra info, aligned with column data
    entity_info: Vec<usize>,
}

pub struct QueryColumn<T: 'static> {
    info: Vec<ArchetypeColumnInfo>,
    data: Vec<AnyVec<dyn Send + Sync>>,
    query_return_tx: mpsc::Sender<QueryReturn>,
    _phantom: PhantomData<T>,
}

impl<T: 'static> QueryColumn<T> {
    fn new(query_return_tx: mpsc::Sender<QueryReturn>) -> Self {
        Self {
            info: Default::default(),
            data: Default::default(),
            query_return_tx,
            _phantom: Default::default(),
        }
    }
}

impl<T: 'static> Drop for QueryColumn<T> {
    fn drop(&mut self) {
        if self.query_return_tx.is_closed() {
            log::warn!(
                "a query was dropped after the AllArchetypes struct it was made from dropped, \
                 this will result in a loss of component data"
            );
        } else {
            let ty = TypeId::of::<T>();
            let info = std::mem::take(&mut self.info);
            let data = std::mem::take(&mut self.data);
            self.query_return_tx
                .try_send(QueryReturn(ty, info, data))
                .unwrap();
        }
    }
}

pub struct QueryColumnIter<'a, T: 'static>(
    Option<std::slice::Iter<'a, T>>,
    Vec<AnyVecRef<'a, T, Heap>>,
);

impl<'a, T: 'static> Iterator for QueryColumnIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_none() {
            if self.1.is_empty() {
                return None;
            } else {
                self.0 = self.1.pop().map(|v| v.iter());
            }
        }

        let iter = self.0.as_mut().unwrap();
        iter.next()
    }
}

pub struct QueryColumnMut<T: 'static>(QueryColumn<T>);

pub struct QueryColumnIterMut<'a, T: 'static>(
    Option<std::slice::IterMut<'a, T>>,
    Vec<AnyVecMut<'a, T, Heap>>,
);

impl<'a, T: 'static> Iterator for QueryColumnIterMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_none() {
            if self.1.is_empty() {
                return None;
            } else {
                self.0 = self.1.pop().map(|mut v| v.iter_mut());
            }
        }

        let iter = self.0.as_mut().unwrap();
        iter.next()
    }
}

pub trait IsQuery {
    type Columns;
    type Item<'t>;
    type ColumnsIter<'t>: Iterator<Item = Self::Item<'t>>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Option<Self::Columns>;
    fn columns_iter(columns: &mut Self::Columns) -> Self::ColumnsIter<'_>;
    fn column_borrows() -> Vec<Borrow>;
}

impl IsQuery for () {
    type Columns = ();
    type Item<'t> = ();
    type ColumnsIter<'t> = std::iter::Cycle<std::iter::Once<()>>;

    fn pop_columns(_: &mut AllArchetypes) -> Option<Self::Columns> {
        Some(())
    }

    fn columns_iter(_: &mut Self::Columns) -> Self::ColumnsIter<'_> {
        std::iter::once(()).cycle()
    }

    fn column_borrows() -> Vec<Borrow> {
        vec![]
    }
}

impl<'a, T: Send + Sync + 'static> IsQuery for &'a T {
    type Columns = QueryColumn<T>;
    type Item<'t> = &'t T;
    type ColumnsIter<'t> = QueryColumnIter<'t, T>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Option<Self::Columns> {
        Some(archetypes.get_column::<T>()?)
    }

    fn columns_iter(cols: &mut Self::Columns) -> Self::ColumnsIter<'_> {
        let mut vs = cols
            .data
            .iter()
            .map(|v| v.downcast_ref().unwrap())
            .collect::<Vec<_>>();
        let first = vs.pop().map(|v| v.iter());
        QueryColumnIter(first, vs)
    }

    fn column_borrows() -> Vec<Borrow> {
        vec![Borrow{ id: ResourceId::new::<QueryColumn<T>>(), is_exclusive: false}]
    }
}

impl<'a, T: Send + Sync + 'static> IsQuery for &'a mut T {
    type Columns = QueryColumnMut<T>;
    type Item<'t> = &'t mut T;
    type ColumnsIter<'t> = QueryColumnIterMut<'t, T>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Option<Self::Columns> {
        Some(archetypes.get_column_mut::<T>()?)
    }

    fn columns_iter(cols: &mut Self::Columns) -> Self::ColumnsIter<'_> {
        let mut vs = cols
            .0
            .data
            .iter_mut()
            .map(|v| v.downcast_mut().unwrap())
            .collect::<Vec<_>>();
        let first = vs.pop().map(|mut v| v.iter_mut());
        QueryColumnIterMut(first, vs)
    }

    fn column_borrows() -> Vec<Borrow> {
        vec![Borrow{id: ResourceId::new::<QueryColumnMut<T>>(), is_exclusive: true}]
    }
}

impl<Head, Tail> IsQuery for (Head, Tail)
where
    Head: IsQuery,
    Tail: IsQuery + TupleList,
{
    type Columns = (Head::Columns, Tail::Columns);
    type Item<'a> = (Head::Item<'a>, Tail::Item<'a>);
    type ColumnsIter<'a> = std::iter::Zip<Head::ColumnsIter<'a>, Tail::ColumnsIter<'a>>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Option<Self::Columns> {
        let head = Head::pop_columns(archetypes)?;
        let tail = Tail::pop_columns(archetypes)?;
        Some((head, tail))
    }

    fn columns_iter(cols: &mut Self::Columns) -> Self::ColumnsIter<'_> {
        Head::columns_iter(&mut cols.0).zip(Tail::columns_iter(&mut cols.1))
    }

    fn column_borrows() -> Vec<Borrow> {
        let mut borrows = Tail::column_borrows();
        borrows.extend(Head::column_borrows());
        borrows
    }
}

pub struct Query<T>(<T::TupleList as IsQuery>::Columns)
where
    T: Tuple + ?Sized,
    T::TupleList: IsQuery;

impl<T> CanFetch for Query<T>
where
    T: Tuple + ?Sized,
    T::TupleList: IsQuery,
{
    fn borrows() -> Vec<Borrow> {
        <T::TupleList as IsQuery>::column_borrows()
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(crate::ResourceId, crate::Resource)>,
        fields: &mut FxHashMap<crate::ResourceId, crate::FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        todo!()
    }
}

impl<'a, T> TryFrom<&'a mut AllArchetypes> for Query<T>
where
    T: Tuple + ?Sized,
    T::TupleList: IsQuery,
{
    type Error = anyhow::Error;

    fn try_from(store: &'a mut AllArchetypes) -> Result<Self, Self::Error> {
        let columns = <T::TupleList as IsQuery>::pop_columns(store).context("missing columns")?;
        Ok(Query(columns))
    }
}

impl<T> Query<T>
where
    T: Tuple + ?Sized,
    T::TupleList: IsQuery,
    for<'a> <T::TupleList as IsQuery>::Item<'a>: TupleList,
{
    pub fn iter_mut<'a>(
        &'a mut self,
    ) -> impl Iterator<Item = <<T::TupleList as IsQuery>::Item<'a> as TupleList>::Tuple> {
        <T::TupleList as IsQuery>::columns_iter(&mut self.0).map(|tlist| tlist.into_tuple())
    }
}

#[cfg(test)]
mod test {
    use std::any::Any;

    use super::*;

    #[test]
    fn archetype_can_create_add_remove_get_mut() {
        let mut arch = Archetype::new::<(f32, &'static str, u32)>().unwrap();
        assert!(arch.insert(0, (0.0f32, "zero", 0u32)).unwrap().is_none());
        assert!(arch.insert(1, (1.0f32, "one", 1u32)).unwrap().is_none());
        assert!(arch.insert(2, (2.0f32, "two", 2u32)).unwrap().is_none());
        assert!(arch.insert(3, (3.0f32, "three", 3u32)).unwrap().is_none());
        assert_eq!(Some((1.0f32,)), arch.insert(1, (111.0f32,)).unwrap());
        assert_eq!((111.0f32, "one", 1u32), arch.remove(1).unwrap().unwrap());
        assert_eq!(
            Some(&3.0f32),
            arch.get(3).unwrap(),
            "{:#?}",
            arch.entity_lookup
        );
        let zero_str: &mut &'static str = arch.get_mut(0).unwrap().unwrap();
        *zero_str = "000";
        drop(zero_str);
        assert_eq!(
            Some(&"000"),
            arch.get::<&'static str>(0).unwrap(),
            "{:#?}",
            arch.entity_lookup
        );
    }

    #[test]
    fn archetype_can_match() {
        let arch = Archetype::new::<(f32, bool)>().unwrap();
        assert!(arch.matches_bundle::<(f32, bool)>().unwrap());
        assert!(arch.matches_bundle::<(bool, f32)>().unwrap());
        assert!(!arch.matches_bundle::<(String, bool)>().unwrap());
        assert!(!arch.matches_bundle::<(f32, bool, String)>().unwrap());
        assert!(!arch.matches_bundle::<(f32,)>().unwrap());
    }

    #[test]
    fn all_archetypes_can_create_add_remove_get_mut() {
        let mut arch = AllArchetypes::default();
        assert!(arch.insert(0, 0.0f32).is_none());
        assert!(arch.insert(1, 1.0f32).is_none());
        assert!(arch.insert(2, 2.0f32).is_none());
        assert!(arch.insert(3, 3.0f32).is_none());
        assert!(arch.insert(1, "one".to_string()).is_none());
        assert!(arch.insert(2, "two".to_string()).is_none());
        assert_eq!(arch.insert(3, 3.33f32).unwrap(), 3.00);
    }

    #[test]
    fn archetype_sanity() {
        let mut any1: AnyVec = AnyVec::new::<f32>();
        let mut any2: AnyVec = AnyVec::new::<f32>();
        {
            let _: std::vec::IntoIter<AnyVecRef<f32, Heap>> = vec![
                any1.downcast_ref::<f32>().unwrap(),
                any2.downcast_ref::<f32>().unwrap(),
            ]
            .into_iter();
        }
        {
            let _: std::vec::IntoIter<AnyVecMut<f32, Heap>> = vec![
                any1.downcast_mut::<f32>().unwrap(),
                any2.downcast_mut::<f32>().unwrap(),
            ]
            .into_iter();
        }
    }

    #[test]
    fn archetype_iter_types() {
        let mut store = AllArchetypes::default();
        store.insert_bundle(0, (0.0f32, "zero".to_string(), false));
        store.insert_bundle(1, (1.0f32, "one".to_string(), false));
        store.insert_bundle(2, (2.0f32, "two".to_string(), false));
        {
            let mut query: Query<(&f32,)> = Query::try_from(&mut store).unwrap();
            let f32s = query.iter_mut().map(|(f,)| *f).collect::<Vec<_>>();
            assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s);
        }
        {
            let mut query: Query<(&mut bool, &f32)> = Query::try_from(&mut store).unwrap();

            for (i, (is_on, f)) in query.iter_mut().enumerate() {
                *is_on = !*is_on;
                assert!(*is_on);
                assert!(*f as usize == i);
            }
        }

        {
            let mut query: Query<(&String, &mut bool, &mut f32)> =
                Query::try_from(&mut store).unwrap();
            for (s, is_on, f) in query.iter_mut() {
                *is_on = true;
                *f = s.len() as f32;
            }
        }

        {
            let mut query: Query<(&bool, &f32, &String)> = Query::try_from(&mut store).unwrap();
            for (is_on, f, s) in query.iter_mut() {
                assert!(is_on);
                assert_eq!(*f, s.len() as f32);
            }
        }
    }

    #[test]
    fn archetype_send_sync() {
        let _: Box<dyn Any + Send + Sync + 'static> = Box::new(AllArchetypes::default());
    }

    #[test]
    fn can_remove_all() {
        let mut all = AllArchetypes::default();

        for id in 0..10000 {
            all.insert(id, id);
        }
        for id in 0..10000 {
            all.insert(id, id as f32);
        }
        for id in 0..10000 {
            let _ = all.remove_any(id);
        }
        assert!(all.is_empty());
    }
}
