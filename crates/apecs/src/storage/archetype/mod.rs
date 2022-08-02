//! Entity components stored together in contiguous arrays.
use std::{any::TypeId, marker::PhantomData, ops::DerefMut, sync::Arc};

use any_vec::{any_value::AnyValueMut, mem::Heap, traits::*, AnyVec, AnyVecMut, AnyVecRef};
use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

mod bundle;
pub use bundle::*;
use smallvec::SmallVec;
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
    pub(crate) types: SmallVec<[TypeId; 4]>,
    pub(crate) data: SmallVec<[Option<AnyVec<dyn Send + Sync + 'static>>; 4]>,
    pub(crate) entity_info: SmallVec<[Option<Vec<EntityInfo>>; 4]>,
    pub(crate) loaned_data: SmallVec<[Option<Arc<InnerColumn>>; 4]>,
    // map of entity_id to storage index
    pub(crate) entity_lookup: FxHashMap<usize, usize>,
}

#[derive(Default)]
pub struct ArchetypeBuilder {
    entities: Vec<EntityInfo>,
    archetype: Archetype,
}

impl ArchetypeBuilder {
    pub fn with_components<T: Send + Sync + 'static>(
        mut self,
        starting_entity: usize,
        comps: impl ExactSizeIterator<Item = T>,
    ) -> Self {
        let mut anyvec: AnyVec<dyn Sync + Send + 'static> = AnyVec::with_capacity::<T>(comps.len());
        let entities = (starting_entity..starting_entity + comps.len())
            .map(|id| EntityInfo::new(id))
            .collect();
        {
            let mut tvec = anyvec.downcast_mut::<T>().unwrap();
            comps.for_each(|c| tvec.push(c));
        }

        let ty = anyvec.element_typeid();
        for (i, t) in self.archetype.types.iter().enumerate() {
            if &ty < t {
                self.archetype.types.insert(i, ty);
                self.archetype.data.insert(i, Some(anyvec));
                self.archetype.loaned_data.insert(i, None);
                self.archetype.entity_info.insert(i, Some(entities));
                return self;
            } else if &ty == t {
                self.archetype.data[i] = Some(anyvec);
                self.archetype.entity_info[i] = Some(entities);
                self.archetype.loaned_data[i] = None;
                return self;
            }
        }

        self.archetype.types.push(ty);
        self.archetype.data.push(Some(anyvec));
        self.archetype.loaned_data.push(None);
        self.archetype.entity_info.push(Some(entities));

        self
    }

    pub fn build(mut self) -> Archetype {
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

impl Archetype {
    fn try_from_bundle(may_entity_id: Option<usize>, bundle: AnyBundle) -> anyhow::Result<Self> {
        let types = bundle.0;
        let (entities, entity_lookup) = if let Some(entity_id) = may_entity_id {
            (
                vec![EntityInfo::new(entity_id)],
                FxHashMap::from_iter([(entity_id, 0)]),
            )
        } else {
            (vec![], FxHashMap::default())
        };
        Ok(Archetype {
            data: bundle.1.into_iter().map(Option::Some).collect(),
            entity_info: types.iter().map(|_| Some(entities.clone())).collect(),
            loaned_data: types.iter().map(|_| None).collect(),
            entity_lookup,
            types,
        })
    }

    pub fn new<B>() -> anyhow::Result<Self>
    where
        B: Tuple + IsBundleTypeInfo + 'static,
        B::TupleList: EmptyBundle,
    {
        let bundle = <B::TupleList as EmptyBundle>::empty_bundle();
        Self::try_from_bundle(None, bundle)
    }

    pub fn len(&self) -> usize {
        self.entity_lookup.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entity_lookup.is_empty()
    }

    fn index_of(&self, ty: &TypeId) -> Option<usize> {
        for (i, t) in self.types.iter().enumerate() {
            if ty == t {
                return Some(i);
            }
        }
        None
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
        let types = bundle::type_info::<B>()?;
        Ok(self.types == types)
    }

    /// Whether this archetype's types contain all of the bundle's component
    /// types.
    ///
    /// # NOTE:
    /// `types` must be ordered, ascending
    pub fn contains_bundle_components(&self, types: &[TypeId]) -> bool {
        let mut here = self.types.iter();
        for ty in types.iter() {
            if here.find(|t| t == &ty).is_none() {
                return false;
            }
        }
        true
    }

    fn get_column<T: 'static>(&self) -> anyhow::Result<AnyVecRef<'_, T, Heap>> {
        let ty = TypeId::of::<T>();
        let i = self.index_of(&ty).context("missing column")?;
        let column = self.data[i]
            .as_ref()
            .or_else(|| self.loaned_data[i].as_ref().map(|innercol| &innercol.0))
            .with_context(|| format!("missing column '{}' data", std::any::type_name::<T>()))?;
        column
            .downcast_ref::<T>()
            .with_context(|| format!("could not downcast to {}", std::any::type_name::<T>()))
    }

    fn get_column_mut<T: 'static>(
        &mut self,
    ) -> anyhow::Result<(AnyVecMut<'_, T, Heap>, &mut [EntityInfo])> {
        self.unify_resources();
        let ty = TypeId::of::<T>();
        let index = self
            .index_of(&ty)
            .with_context(|| format!("no such column {}", std::any::type_name::<T>()))?;
        let column = self.data[index]
            .as_mut()
            .with_context(|| format!("column '{}' is borrowed", std::any::type_name::<T>()))?;
        let ents = self.entity_info[index]
            .as_mut()
            .with_context(|| format!("column '{}' info is borrowed", std::any::type_name::<T>()))?
            .as_mut_slice();
        Ok((
            column
                .downcast_mut::<T>()
                .with_context(|| format!("could not downcast to {}", std::any::type_name::<T>()))?,
            ents,
        ))
    }

    fn borrow_column(&mut self, ty: &TypeId) -> Option<Arc<InnerColumn>> {
        let i = self.index_of(ty)?;

        let col = || -> Option<_> {
            let data = self.data[i].take()?;
            let info = self.entity_info[i].take()?;
            let col = Arc::new(InnerColumn(data, info));
            self.loaned_data[i] = Some(col.clone());
            Some(col)
        }()
        .or_else(|| self.loaned_data[i].clone());
        col
    }

    fn unify_resources(&mut self) {
        log::trace!("Archetype::unify_resources types: {:?}", self.types);
        let indices = self
            .loaned_data
            .iter()
            .enumerate()
            .filter_map(|(i, may_col)| {
                let col = may_col.as_ref()?;
                if Arc::strong_count(&col) == 1 {
                    Some(i)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        log::trace!("archetype is unifying {} loaned resources", indices.len());

        for i in indices.into_iter() {
            // these ops are ok because we already know these are `Some(_)` and we have
            // already checked the `strong_count`
            let col: Option<InnerColumn> = self.loaned_data[i]
                .take()
                .map(|arc| Arc::try_unwrap(arc).unwrap());
            debug_assert!(col.is_some());
            debug_assert!(self.data[i].is_none());
            debug_assert!(self.entity_info[i].is_none());
            let col = col.unwrap();
            self.data[i] = Some(col.0);
            self.entity_info[i] = Some(col.1);
        }
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

    /// Insert a component bundle into the archetype.
    ///
    /// Returns any previously stored bundle components.
    ///
    /// Errs if the bundle types do not match the archetype.
    pub fn insert_any(
        &mut self,
        entity_id: usize,
        mut bundle: AnyBundle,
    ) -> anyhow::Result<Option<AnyBundle>> {
        Ok(
            if let Some(entity_index) = self.entity_lookup.get(&entity_id) {
                for (ty, component_vec) in bundle.0.iter().zip(bundle.1.iter_mut()) {
                    let ty_index = self.index_of(ty).context("missing column")?;
                    let info = self.entity_info[ty_index]
                        .as_mut()
                        .context("column is borrowed")?;
                    info[*entity_index].mark_changed();
                    let data = self.data[ty_index].as_mut().context("column is borrowed")?;
                    let mut element_a = data.at_mut(*entity_index);
                    let mut element_b = component_vec.at_mut(0);
                    element_a.swap(element_b.deref_mut());
                }
                Some(bundle)
            } else {
                let last_index = self.entity_lookup.len();
                assert!(self.entity_lookup.insert(entity_id, last_index).is_none());
                for (ty, mut component_vec) in bundle.0.into_iter().zip(bundle.1.into_iter()) {
                    let ty_index = self.index_of(&ty).context("missing column")?;
                    let data = self.data[ty_index].as_mut().context("column is borrowed")?;
                    let info = self.entity_info[ty_index]
                        .as_mut()
                        .context("column is borrowed")?;
                    data.push(component_vec.pop().unwrap());
                    debug_assert!(data.len() == last_index + 1);
                    info.push(EntityInfo::new(entity_id));
                    debug_assert!(info.len() == last_index + 1);
                }
                None
            },
        )
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
            let mut anybundle = AnyBundle::default();
            anybundle.0 = self.types.clone();
            for (column, info) in self.data.iter_mut().zip(self.entity_info.iter_mut()) {
                let data = column.as_mut().context("column is borrowed")?;
                let info = info.as_mut().context("column is borrowed")?;
                assert_eq!(info.swap_remove(index).key, entity_id);
                if should_reset_index {
                    // the index of the last entity was swapped for that of the removed
                    // index, so we need to update the lookup
                    self.entity_lookup.insert(info[index].key, index);
                    should_reset_index = false;
                }

                let mut temp_store = data.clone_empty();
                debug_assert!(
                    index < data.len(),
                    "store len {} does not contain index {}",
                    data.len(),
                    index
                );
                let value = data.swap_remove(index);
                temp_store.push(value);
                anybundle.1.push(temp_store);
            }
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
        log::trace!("AllArchetypes::unify_resources");
        // store the indices of any archetypes that have already had their resources unified
        let mut indices = FxHashSet::default();
        while let Some(QueryReturn(ty, infos, data)) = self.query_return_chan.1.try_recv().ok() {
            for (ArchetypeIndex(archetype_i), col) in infos.into_iter().zip(data.into_iter()) {
                let archetype = &mut self.archetypes[archetype_i];
                let i = archetype.index_of(&ty).unwrap();
                debug_assert!(
                    archetype.data[i].is_none(),
                    "inserted query return data into archetype that already had data"
                );
                archetype.data[i] = Some(col.0);
                archetype.entity_info[i] = Some(col.1);
                archetype.unify_resources();
                indices.insert(archetype_i);
            }
        }
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if indices.contains(&i) {
                continue;
            }

            arch.unify_resources();
        }
    }

    pub fn get_column<T: 'static>(&mut self) -> Option<QueryColumn<T>> {
        self.unify_resources();
        let ty = TypeId::of::<T>();
        let mut column = QueryColumn::<T>::default();
        for arch in self.archetypes.iter_mut() {
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

    /// Attempt to obtain a query column of the given type.
    ///
    /// ## PANICS
    /// Panics if the requested column exists, but is already borrowed.
    pub fn get_column_mut<T: 'static>(&mut self) -> Option<QueryColumnMut<T>> {
        self.unify_resources();
        let ty = TypeId::of::<T>();
        let mut column = QueryColumnMut::<T>::new(self.query_return_chan.0.clone());
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if let Some(index) = arch.index_of(&ty) {
                let data = arch.data[index].take().expect("column is borrowed");
                let info = arch.entity_info[index].take().expect("column info is borrowed");
                let col = InnerColumn(data, info);
                column.indices.push(ArchetypeIndex(i));
                column.column.push(col);
            }
        }
        if column.column.is_empty() {
            None
        } else {
            Some(column)
        }
    }

    /// Attempts to obtain a mutable reference to an archetype that exactly matches the
    /// given types.
    ///
    /// ## NOTE:
    /// The types given must be ordered, ascending.
    pub fn get_archetype_mut(&mut self, types: &[TypeId]) -> Option<&mut Archetype> {
        for arch in self.archetypes.iter_mut() {
            if arch.types.as_slice() == types {
                return Some(arch);
            }
        }
        None
    }

    pub fn insert_archetype(&mut self, archetype: Archetype) {
        if let Some(_) = self.get_archetype_mut(&archetype.types) {
            panic!("archetype already exists");
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
        let mut bundle_types: SmallVec<[TypeId; 4]> = bundle::type_info::<B>().unwrap();
        // The bundle we intend to store. This bundle's types may not match
        // output_bundle after the first step, because we may have found an
        // existing bundle at that entity and now have to store the union of the
        // two in a new archetype
        let mut input_bundle = AnyBundle::from_tuple(bundle);
        // The output bundle we indend to give back to the caller
        let mut output_bundle: Option<AnyBundle> = None;

        // first find if the entity already exists
        for arch in self.archetypes.iter_mut() {
            if arch.contains_entity(&entity_id) {
                if bundle_types == arch.types {
                    // we found a perfect match, simply store the bundle,
                    // returning the previous components
                    let prev = arch.insert_any(entity_id, input_bundle).unwrap().unwrap();
                    return Some(prev.try_into_tuple().unwrap());
                }
                // otherwise we remove the bundle from the archetype, adding the
                // non-overlapping key+values to our bundle
                let mut old_bundle = arch.remove_any(entity_id).unwrap().unwrap();
                output_bundle = old_bundle.union(input_bundle);
                input_bundle = old_bundle;
                bundle_types = input_bundle.0.clone();
                break;
            }
        }

        // now search for an archetype to store the bundle
        for arch in self.archetypes.iter_mut() {
            if arch.contains_bundle_components(&bundle_types) {
                // we found an archetype that will store our bundle, so store it
                let no_previous_bundle =
                    arch.insert_any(entity_id, input_bundle).unwrap().is_none();
                assert!(
                    no_previous_bundle,
                    "the archetype had a bundle at the entity"
                );
                return None;
            }
        }

        // we couldn't find any existing archetypes that can hold the bundle, so make a
        // new one
        let new_arch = Archetype::try_from_bundle(Some(entity_id), input_bundle).unwrap();
        self.archetypes.push(new_arch);
        output_bundle.and_then(|b| b.try_into_tuple().ok())
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
                    let prev = bundle.remove(&ty).map(|b| {
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
            if arch.types.as_slice() == bundle_types {
                // we found the archetype that can store our bundle, so store it
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
        let new_arch = Archetype::try_from_bundle(Some(entity_id), bundle).unwrap();
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
    indices: Vec<ArchetypeIndex>,
    column: Vec<InnerColumn>,
    query_return_tx: mpsc::Sender<QueryReturn>,
    _phantom: PhantomData<T>,
}

impl<T: 'static> Default for QueryColumnMut<T> {
    fn default() -> Self {
        Self {
            indices: Default::default(),
            column: Default::default(),
            query_return_tx: mpsc::bounded(1).0,
            _phantom: Default::default(),
        }
    }
}

impl<T: 'static> QueryColumnMut<T> {
    fn new(query_return_tx: mpsc::Sender<QueryReturn>) -> Self {
        Self {
            indices: Default::default(),
            column: Default::default(),
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
            let info = std::mem::take(&mut self.indices);
            let data = std::mem::take(&mut self.column);
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
        cols.column
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
        let bundle = super::bundle::empty_bundle::<(f32, bool)>();
        let arch = Archetype::new::<(f32, bool)>().unwrap();
        assert!(!arch.types.is_empty(), "archetype types is empty: {:?}", bundle.0);
        fn assert_matches<B: IsBundleTypeInfo + 'static>(a: &Archetype) {
            match a.matches_bundle::<B>() {
                Ok(does) => assert!(does, "{:?} != {:?}", super::bundle::type_info::<B>().unwrap(), a.types),
                Err(e) => panic!("{}", e),
            }
        }
        assert_matches::<(f32, bool)>(&arch);
        assert_matches::<(bool, f32)>(&arch);
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
        println!("insert");
        let mut store = AllArchetypes::default();
        store.insert_bundle(0, (0.0f32, "zero".to_string(), false));
        store.insert_bundle(1, (1.0f32, "one".to_string(), false));
        store.insert_bundle(2, (2.0f32, "two".to_string(), false));
        {
            println!("query 1");
            let mut query: Query<(&f32,)> = Query::try_from(&mut store).unwrap();
            let f32s = query.run().map(|(f,)| *f).collect::<Vec<_>>();
            let f32s_again = query.run().map(|(f,)| *f).collect::<Vec<_>>();
            assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s);
            assert_eq!(vec![0.0f32, 1.0, 2.0f32], f32s_again);
        }
        {
            println!("query 2");
            let mut query: Query<(&mut bool, &f32)> = Query::try_from(&mut store).unwrap();

            for (i, (mut is_on, f)) in query.run().enumerate() {
                *is_on = !*is_on;
                assert!(*is_on);
                assert!(*f as usize == i);
            }
        }

        {
            println!("query 3");
            let mut query: Query<(&String, &mut bool, &mut f32)> =
                Query::try_from(&mut store).unwrap();
            for (s, mut is_on, mut f) in query.run() {
                *is_on = true;
                *f = s.len() as f32;
            }
        }

        {
            println!("query 4");
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
            log::debug!("fetching query 1");
            let mut query = world.fetch::<Query<(&f32, &bool)>>().unwrap();
            log::debug!("using query 1.1");
            let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f, *b)).unzip();
            assert_eq!(vec![0.0, 1.0, 2.0], f32s);
            assert_eq!(vec![true, false, true], bools);
            // do it again
            log::debug!("using query 1.2");
            let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f, *b)).unzip();
            assert_eq!(vec![0.0, 1.0, 2.0], f32s);
            assert_eq!(vec![true, false, true], bools);
        }
        {
            log::debug!("fetching query 2");
            let mut query = world.fetch::<Query<(&mut f32, &mut bool)>>().unwrap();
            log::debug!("using query 2.1");
            for (mut f, mut b) in query.run() {
                *f *= 3.0;
                *b = *f < 5.0;
            }
            log::debug!("using query 2.2");
            let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f, *b)).unzip();
            assert_eq!(
                (vec![0.0, 3.0, 6.0], vec![true, true, false]),
                (f32s, bools)
            );
        }
        {
            log::debug!("fetching query 3");
            let mut query = world.fetch::<Query<(&f32, &bool)>>().unwrap();
            log::debug!("using query 3.1");
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

        let n = 10_000;
        let mut all = AllArchetypes::default();
        let archetype = ArchetypeBuilder::default()
            .with_components(0, (0..n).map(|_| A(0.0)))
            .build();
        all.insert_archetype(archetype);

        for id in 0..n {
            let _ = all.insert(id, B(0.0));
        }

        for id in 0..n {
            let _ = all.remove_component::<B>(id);
        }
    }
}
