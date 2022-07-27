//! Entity components stored together in contiguous arrays.
use std::{
    any::TypeId,
    marker::PhantomData,
    ops::{DerefMut, Range},
    sync::Arc,
};

use any_vec::{any_value::AnyValueMut, mem::Heap, traits::*, AnyVec, AnyVecMut, AnyVecRef};
use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

mod bundle;
pub use bundle::*;
use tuple_list::TupleList;

use crate::{mpsc, resource_manager::LoanManager, schedule::Borrow, CanFetch, ResourceId};

use super::{EntityInfo, HasEntityInfoMut, Mut, Ref};

#[derive(Debug)]
pub struct InnerColumn(AnyVec<dyn Sync + Send + 'static>, Vec<EntityInfo>);

impl InnerColumn {
    pub fn iter<'a, T: 'static>(&'a self) -> InnerColumnIter<'a, T> {
        self.0
            .downcast_ref::<T>()
            .unwrap()
            .into_iter()
            .zip(self.1.iter())
            .map(|(c, e)| Ref(e, c))
    }

    pub fn iter_mut<'a, T: 'static>(&'a mut self) -> InnerColumnIterMut<'a, T> {
        self.0
            .downcast_mut::<T>()
            .unwrap()
            .into_iter()
            .zip(self.1.iter_mut())
            .map(|(c, e)| Mut(e, c))
    }
}

pub type InnerColumnIter<'a, T> = std::iter::Map<
    std::iter::Zip<std::slice::Iter<'a, T>, std::slice::Iter<'a, EntityInfo>>,
    fn((&'a T, &'a EntityInfo)) -> Ref<'a, T>,
>;

pub type InnerColumnIterMut<'a, T> = std::iter::Map<
    std::iter::Zip<std::slice::IterMut<'a, T>, std::slice::IterMut<'a, EntityInfo>>,
    fn((&'a mut T, &'a mut EntityInfo)) -> Mut<'a, T>,
>;

/// A collection of entities having the same component types
#[derive(Debug, Default)]
pub struct Archetype {
    pub(crate) types: FxHashSet<TypeId>,
    // map of entity_id to storage index
    pub(crate) entity_lookup: FxHashMap<usize, usize>,
    pub(crate) data: FxHashMap<TypeId, InnerColumn>,
    pub(crate) loaned_data: FxHashMap<TypeId, Arc<InnerColumn>>,
}

#[derive(Default)]
pub struct ArchetypeBuilder {
    entities: Vec<EntityInfo>,
    archetype: Archetype,
}

impl ArchetypeBuilder {
    pub fn with_entities(mut self, entities: impl Iterator<Item = EntityInfo>) -> Self {
        self.entities = entities.collect();
        self
    }

    pub fn with_components<T: Send + Sync + 'static>(
        mut self,
        comps: impl ExactSizeIterator<Item = T>,
    ) -> Self {
        let ty = TypeId::of::<T>();
        let mut anyvec: AnyVec<dyn Sync + Send + 'static> = AnyVec::with_capacity::<T>(comps.len());
        {
            let mut tvec = anyvec.downcast_mut::<T>().unwrap();
            comps.for_each(|c| tvec.push(c));
        }
        let _ = self.archetype.data.insert(ty, InnerColumn(anyvec, vec![]));
        self
    }

    pub fn build(mut self) -> Archetype {
        for col in self.archetype.data.values_mut() {
            col.1 = self.entities.clone();
        }
        self.archetype.entity_lookup =
            FxHashMap::from_iter(self.entities.into_iter().map(|e| e.key).zip(0..));

        self.archetype
    }
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
        let _ = archetype.data.insert(ty, InnerColumn(anyvec, vec![]));
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
            data: bundle
                .0
                .into_iter()
                .map(|(ty, v)| (ty, InnerColumn(v, vec![])))
                .collect(),
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
        self.entity_lookup.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entity_lookup.is_empty()
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

    fn append(&mut self, mut archetype: Archetype) -> anyhow::Result<()> {
        for (ty, col) in self.data.iter_mut() {
            let mut acol = archetype.data.remove(ty).context("merge missing col")?;
            while let Some(comp) = acol.0.pop() {
                col.0.push(comp);
            }
            col.1.extend(acol.1);
        }

        anyhow::ensure!(
            archetype.data.is_empty(),
            "merged archetype has unmerged components"
        );

        Ok(())
    }

    fn get_column<T: 'static>(&self) -> anyhow::Result<AnyVecRef<'_, T, Heap>> {
        let ty = TypeId::of::<T>();
        let column = self
            .data
            .get(&ty)
            .or_else(|| self.loaned_data.get(&ty).map(AsRef::as_ref))
            .with_context(|| format!("no such column {}", std::any::type_name::<T>()))?;
        column
            .0
            .downcast_ref::<T>()
            .with_context(|| format!("could not downcast to {}", std::any::type_name::<T>()))
    }

    fn get_column_mut<T: 'static>(
        &mut self,
    ) -> anyhow::Result<(AnyVecMut<'_, T, Heap>, &mut [EntityInfo])> {
        self.unify_resources();
        let ty = TypeId::of::<T>();
        let column = self
            .data
            .get_mut(&ty)
            .with_context(|| format!("no such column {}", std::any::type_name::<T>()))?;
        Ok((
            column
                .0
                .downcast_mut::<T>()
                .with_context(|| format!("could not downcast to {}", std::any::type_name::<T>()))?,
            column.1.as_mut_slice(),
        ))
    }

    fn borrow_column(&mut self, ty: &TypeId) -> Option<Arc<InnerColumn>> {
        log::trace!("borrow {:?}", ty);
        match self.data.remove(ty) {
            Some(col) => {
                let col = Arc::new(col);
                self.loaned_data.insert(*ty, col.clone());
                Some(col)
            }
            None => self.loaned_data.get(ty).cloned(),
        }
    }

    fn unify_resources(&mut self) {
        let tys = self
            .loaned_data
            .iter()
            .filter_map(|(ty, col)| {
                if Arc::strong_count(&col) == 1 {
                    Some(*ty)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        log::trace!("archetype is unifying {} resources", tys.len());

        for ty in tys.into_iter() {
            // these ops are ok because we already checked the strong count
            let col: Arc<_> = self.loaned_data.remove(&ty).unwrap();
            let col: InnerColumn = Arc::try_unwrap(col).unwrap();
            let prev = self.data.insert(ty, col);
            debug_assert!(prev.is_none());
        }
    }

    fn take_column(&mut self, ty: &TypeId) -> Option<InnerColumn> {
        self.unify_resources();
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
                let mut element_a = column.0.at_mut(*index);
                let mut element_b = component_vec.at_mut(0);
                element_a.swap(element_b.deref_mut());
            }
            Some(bundle)
        } else {
            let last_index = self.entity_lookup.len();
            assert!(self.entity_lookup.insert(entity_id, last_index).is_none());
            for (ty, mut component_vec) in bundle.0.into_iter() {
                let column = self.data.get_mut(&ty).context("missing column")?;
                column.0.push(component_vec.pop().unwrap());
                debug_assert!(column.0.len() == last_index + 1);
                column.1.push(EntityInfo::new(entity_id));
                debug_assert!(column.1.len() == last_index + 1);
            }
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
            // NOTE: last_index is the length of entity_lookup because we removed the entity
            // in question already!
            let last_index = self.entity_lookup.len();
            let mut should_reset_index = index != last_index;
            let anybundle =
                self.data
                    .iter_mut()
                    .fold(AnyBundle::default(), |mut bundle, (_ty, column)| {
                        assert_eq!(column.1.swap_remove(index).key, entity_id);
                        if should_reset_index {
                            // the index of the last entity was swapped for that of the removed
                            // index, so we need to update the lookup
                            self.entity_lookup.insert(column.1[index].key, index);
                            should_reset_index = false;
                        }

                        let mut temp_store = column.0.clone_empty();
                        debug_assert!(
                            index < column.0.len(),
                            "store len {} does not contain index {}",
                            column.0.len(),
                            index
                        );
                        let value = column.0.swap_remove(index);
                        temp_store.push(value);
                        bundle.0.insert(column.0.element_typeid(), temp_store);
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
    ///
    /// ## Note
    /// This will mark the component changed.
    pub fn get_mut<T: 'static>(&mut self, entity_id: usize) -> anyhow::Result<Option<&mut T>> {
        if let Some(index) = self.entity_lookup.get(&entity_id).copied() {
            let mut column = self.get_column_mut::<T>()?;
            if let Some(e) = column.1.get_mut(index) {
                e.mark_changed();
                Ok(column.0.get_mut(index))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

struct QueryReturn(TypeId, Vec<ArchetypeIndex>, Vec<InnerColumn>);

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
                let archetype = &mut self.archetypes[info.0];
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
        let mut column = QueryColumn::<T>::default();
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if let Some(col) = arch.borrow_column(&ty) {
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
        self.unify_resources();
        let ty = TypeId::of::<T>();
        let mut column = QueryColumnMut::<T>::new(self.query_return_chan.0.clone()); //
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if let Some(col) = arch.take_column(&ty) {
                column.info.push(ArchetypeIndex(i));
                column.data.push(col);
            }
        }
        if column.data.is_empty() {
            None
        } else {
            Some(column)
        }
    }

    fn find_matching_mut<B: IsBundleTypeInfo + 'static>(&mut self) -> Option<&mut Archetype> {
        let types = B::get_type_info().unwrap();
        self.find_matching_any_mut(&types)
    }

    fn find_matching_any_mut(&mut self, types: &FxHashSet<TypeId>) -> Option<&mut Archetype> {
        for arch in self.archetypes.iter_mut() {
            if &arch.types == types {
                return Some(arch);
            }
        }
        None
    }

    pub fn insert_archetype(&mut self, archetype: Archetype) {
        if let Some(local) = self.find_matching_any_mut(&archetype.types) {
            local.append(archetype).unwrap();
        } else {
            self.archetypes.push(archetype);
        }
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
        B::TupleList: EmptyBundle,
    {
        let mut bundle_types: FxHashSet<TypeId> = bundle::type_info::<B>().unwrap();
        let mut any_bundle = AnyBundle::from_tuple(bundle);
        let mut prev_bundle: Option<AnyBundle> = None;

        // first find if the entity already exists
        for arch in self.archetypes.iter_mut() {
            if arch.contains_entity(&entity_id) {
                if arch.matches_bundle::<B>().unwrap() {
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
        for (_ty, column) in new_arch.data.iter_mut() {
            column.1.push(EntityInfo::new(entity_id));
        }
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
        self.unify_resources();
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
        for (_ty, column) in new_arch.data.iter_mut() {
            column.1.push(EntityInfo::new(entity_id));
        }
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

// This column's archetype index in the AllArchetypes struct
struct ArchetypeIndex(usize);

pub struct QueryColumn<T: 'static> {
    data: Vec<Arc<InnerColumn>>,
    _phantom: PhantomData<T>,
}

impl<T: 'static> Default for QueryColumn<T> {
    fn default() -> Self {
        Self {
            data: Default::default(),
            _phantom: Default::default(),
        }
    }
}

pub type QueryColumnIter<'a, T> = std::iter::FlatMap<
    std::slice::Iter<'a, Arc<InnerColumn>>,
    InnerColumnIter<'a, T>,
    fn(&'a Arc<InnerColumn>) -> InnerColumnIter<'a, T>,
>;

pub struct QueryColumnMut<T: 'static> {
    info: Vec<ArchetypeIndex>,
    data: Vec<InnerColumn>,
    query_return_tx: mpsc::Sender<QueryReturn>,
    _phantom: PhantomData<T>,
}

impl<T: 'static> Default for QueryColumnMut<T> {
    fn default() -> Self {
        Self {
            info: Default::default(),
            data: Default::default(),
            query_return_tx: mpsc::bounded(1).0,
            _phantom: Default::default(),
        }
    }
}

impl<T: 'static> QueryColumnMut<T> {
    fn new(query_return_tx: mpsc::Sender<QueryReturn>) -> Self {
        Self {
            info: Default::default(),
            data: Default::default(),
            query_return_tx,
            _phantom: Default::default(),
        }
    }
}

impl<T: 'static> Drop for QueryColumnMut<T> {
    fn drop(&mut self) {
        if self.query_return_tx.is_closed() {
            log::warn!(
                "a query was dropped after the AllArchetypes struct it was made from dropped, \
                 this will result in a loss of component data"
            );
        } else {
            log::trace!("sending column of {} back", std::any::type_name::<T>());
            let ty = TypeId::of::<T>();
            let info = std::mem::take(&mut self.info);
            let data = std::mem::take(&mut self.data);
            self.query_return_tx
                .try_send(QueryReturn(ty, info, data))
                .unwrap();
        }
    }
}

pub type QueryColumnIterMut<'a, T> = std::iter::FlatMap<
    std::slice::IterMut<'a, InnerColumn>,
    InnerColumnIterMut<'a, T>,
    fn(&'a mut InnerColumn) -> InnerColumnIterMut<'a, T>,
>;

pub trait IsQuery {
    type Columns: Default;
    type Item<'t>;
    type ColumnsIter<'t>: Iterator<Item = Self::Item<'t>>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Self::Columns;
    fn columns_iter(columns: &mut Self::Columns) -> Self::ColumnsIter<'_>;
    fn column_borrows() -> Vec<Borrow>;
}

impl IsQuery for () {
    type Columns = ();
    type Item<'t> = ();
    type ColumnsIter<'t> = std::iter::Cycle<std::iter::Once<()>>;

    fn pop_columns(_: &mut AllArchetypes) -> Self::Columns {
        ()
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
    type Item<'t> = Ref<'t, T>;
    type ColumnsIter<'t> = QueryColumnIter<'t, T>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Self::Columns {
        archetypes.get_column::<T>().unwrap_or_default()
    }

    fn columns_iter(cols: &mut QueryColumn<T>) -> Self::ColumnsIter<'_> {
        cols.data.iter().flat_map(|col| col.iter())
    }

    fn column_borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<QueryColumn<T>>(),
            is_exclusive: false,
        }]
    }
}

impl<'a, T: Send + Sync + 'static> IsQuery for &'a mut T {
    type Columns = QueryColumnMut<T>;
    type Item<'t> = Mut<'t, T>;
    type ColumnsIter<'t> = QueryColumnIterMut<'t, T>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Self::Columns {
        archetypes.get_column_mut::<T>().unwrap_or_default()
    }

    fn columns_iter(cols: &mut QueryColumnMut<T>) -> Self::ColumnsIter<'_> {
        cols.data
            .iter_mut()
            .flat_map((|ic| ic.iter_mut()) as fn(&mut InnerColumn) -> InnerColumnIterMut<'_, T>)
    }

    fn column_borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<QueryColumnMut<T>>(),
            is_exclusive: true,
        }]
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

    fn pop_columns(archetypes: &mut AllArchetypes) -> Self::Columns {
        let head = Head::pop_columns(archetypes);
        let tail = Tail::pop_columns(archetypes);
        (head, tail)
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
    T: Tuple + Send + Sync + ?Sized,
    T::TupleList: IsQuery,
    <T::TupleList as IsQuery>::Columns: Send + Sync + 'static,
{
    fn borrows() -> Vec<Borrow> {
        <T::TupleList as IsQuery>::column_borrows()
    }

    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self> {
        let all = loan_mngr.get_mut::<AllArchetypes>()?;
        Ok(Query::<T>::from(all))
    }
}

impl<'a, T> From<&'a mut AllArchetypes> for Query<T>
where
    T: Tuple + Send + Sync + ?Sized,
    T::TupleList: IsQuery,
{
    fn from(store: &'a mut AllArchetypes) -> Self {
        let columns = <T::TupleList as IsQuery>::pop_columns(store);
        Query(columns)
    }
}

pub type QueryIter<'a, T> = std::iter::Map<
    <<T as Tuple>::TupleList as IsQuery>::ColumnsIter<'a>,
    fn(
        <<T as Tuple>::TupleList as IsQuery>::Item<'a>,
    ) -> <<<T as Tuple>::TupleList as IsQuery>::Item<'a> as TupleList>::Tuple,
>;

pub type QueryIterItem<'a, T> =
    <<<T as Tuple>::TupleList as IsQuery>::Item<'a> as TupleList>::Tuple;

impl<T> Query<T>
where
    T: Tuple + ?Sized,
    T::TupleList: IsQuery,
    for<'a> <T::TupleList as IsQuery>::Item<'a>: TupleList,
{
    pub fn run(&mut self) -> QueryIter<T> {
        <T::TupleList as IsQuery>::columns_iter(&mut self.0).map(|tlist| tlist.into_tuple())
    }
}

#[cfg(test)]
mod test {
    use std::any::Any;

    use crate::{storage::*, world::World};

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
            let f32s = query.run().map(|(f,)| *f).collect::<Vec<_>>();
            let f32s_again = query.run().map(|(f,)| *f).collect::<Vec<_>>();
            assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s);
            assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s_again);
        }
        {
            let mut query: Query<(&mut bool, &f32)> = Query::try_from(&mut store).unwrap();

            for (i, (mut is_on, f)) in query.run().enumerate() {
                *is_on = !*is_on;
                assert!(*is_on);
                assert!(*f as usize == i);
            }
        }

        {
            let mut query: Query<(&String, &mut bool, &mut f32)> =
                Query::try_from(&mut store).unwrap();
            for (s, mut is_on, mut f) in query.run() {
                *is_on = true;
                *f = s.len() as f32;
            }
        }

        {
            let mut query: Query<(&bool, &f32, &String)> = Query::try_from(&mut store).unwrap();
            for (is_on, f, s) in query.run() {
                assert!(*is_on);
                assert_eq!(*f, s.len() as f32, "{}.len() != {}", *s, *f);
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

    #[test]
    fn canfetch_query() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut all = AllArchetypes::default();
        let _ = all.insert_bundle(0, (0.0f32, true));
        let _ = all.insert_bundle(1, (1.0f32, false));
        let _ = all.insert_bundle(2, (2.0f32, true));
        let mut world = World::default();
        world.with_resource(all).unwrap();

        {
            log::debug!("fetching query");
            let mut query = world.fetch::<Query<(&f32, &bool)>>().unwrap();
            log::debug!("using query 1");
            let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f, *b)).unzip();
            assert_eq!(vec![0.0, 1.0, 2.0], f32s);
            assert_eq!(vec![true, false, true], bools);
            // do it again
            log::debug!("using query 2");
            let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f, *b)).unzip();
            assert_eq!(vec![0.0, 1.0, 2.0], f32s);
            assert_eq!(vec![true, false, true], bools);
        }
        {
            let mut query = world.fetch::<Query<(&mut f32, &mut bool)>>().unwrap();
            for (mut f, mut b) in query.run() {
                *f *= 3.0;
                *b = *f < 5.0;
            }
            let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f, *b)).unzip();
            assert_eq!(
                (vec![0.0, 3.0, 6.0], vec![true, true, false]),
                (f32s, bools)
            );
        }
        {
            let mut query = world.fetch::<Query<(&f32, &bool)>>().unwrap();
            let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f, *b)).unzip();
            assert_eq!(
                (vec![0.0, 3.0, 6.0], vec![true, true, false]),
                (f32s, bools)
            );
        }
    }

    #[test]
    fn mut_sanity() {
        let mut a = 0.0f32;
        let mut e = EntityInfo::new(0);
        {
            let mut m = Mut(&mut e, &mut a);
            *m = 1.0;
        }

        assert_eq!(1.0, a);
    }

    #[test]
    fn any_vec_into_iter_sanity() {
        let mut vec: AnyVec<dyn Send + Sync> = AnyVec::new::<f32>();
        {
            let mut v = vec.downcast_mut::<f32>().unwrap();
            v.push(0.0);
            v.push(1.0);
            v.push(2.0);
        }

        vec.downcast_mut::<f32>()
            .unwrap()
            .into_iter()
            .for_each(|f| *f += 1.0);

        {
            let v = vec.downcast_ref::<f32>().unwrap();
            assert_eq!(
                vec![1.0, 2.0, 3.0],
                v.iter().map(|f| *f).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn archetype_add_remove_component() {
        struct A(f32);
        struct B(f32);

        let mut all = AllArchetypes::default();
        let archetype = ArchetypeBuilder::default()
            .with_components((0..10000).map(|_| A(0.0)))
            .with_entities((0..10000).map(|e| EntityInfo::new(e)))
            .build();
        all.insert_archetype(archetype);

        for id in 0..10000 {
            let _ = all.insert(id, B(0.0));
        }

        for id in 0..10000 {
            let _ = all.remove_component::<B>(id);
        }
    }
}
