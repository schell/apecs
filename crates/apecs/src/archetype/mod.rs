//! Entity components stored together in contiguous arrays.
use std::{
    any::{Any, TypeId},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use any_vec::{
    any_value::{AnyValue, AnyValueMut},
    element::{ElementMut, ElementPointer},
    mem::Heap,
    AnyVec, AnyVecMut, AnyVecRef,
};
use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

mod bundle;
pub use bundle::*;

use crate::schedule::IsBatch;

/// A collection of entities having the same component types
#[derive(Default)]
pub struct Archetype {
    types: FxHashSet<TypeId>,
    entities: Vec<usize>,
    // map of entity_id to storage index
    entity_lookup: FxHashMap<usize, usize>,
    data: FxHashMap<TypeId, AnyVec>,
}

impl TryFrom<(usize, AnyBundle)> for Archetype {
    type Error = anyhow::Error;

    fn try_from((entity_id, bundle): (usize, AnyBundle)) -> Result<Self, Self::Error> {
        let mut types = bundle.0.keys().map(Clone::clone).collect::<Vec<_>>();
        types.sort();
        bundle::ensure_type_info(&types)?;
        let types = FxHashSet::from_iter(types);
        Ok(Archetype {
            types,
            entities: vec![entity_id],
            entity_lookup: FxHashMap::from_iter([(entity_id, 0)]),
            data: bundle.0,
        })
    }
}

impl Archetype {
    pub fn new<B: IsBundle + 'static>() -> anyhow::Result<Self> {
        let types = B::type_info()?;
        let mut this = Archetype::default();
        this.types = FxHashSet::from_iter(types.into_iter());
        this.insert_columns_for::<B>()?;
        Ok(this)
    }

    /// Add the given bundle types to this archetype.
    ///
    /// This creates new empty entries in the data map.
    fn insert_columns_for<B2: IsBundle + 'static>(&mut self) -> anyhow::Result<()> {
        if B2::as_root().is_none() {
            let store = AnyVec::new::<B2::Head>();
            anyhow::ensure!(
                self.data.insert(TypeId::of::<B2::Head>(), store).is_none(),
                "attempted to add a type that already exists: {}",
                std::any::type_name::<B2::Head>()
            );
            self.insert_columns_for::<B2::Tail>()?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entities.len()
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
    pub fn matches_bundle<B: IsBundle + 'static>(&self) -> anyhow::Result<bool> {
        let types = FxHashSet::from_iter(B::type_info()?.into_iter());
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

    pub fn insert<B: IsBundle + 'static>(
        &mut self,
        entity_id: usize,
        bundle: B,
    ) -> anyhow::Result<Option<B>> {
        if let Some(prev) = self.insert_any(entity_id, bundle.into_any())? {
            let bundle = B::try_from_any(prev)?;
            Ok(Some(bundle))
        } else {
            Ok(None)
        }
    }

    pub fn insert_any(
        &mut self,
        entity_id: usize,
        mut bundle: AnyBundle,
    ) -> anyhow::Result<Option<AnyBundle>> {
        let prev = if let Some(index) = self.entity_lookup.get(&entity_id) {
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
        };

        Ok(prev)
    }

    /// Remove the entity and return its bundle, if any.
    pub fn remove<B: IsBundle + 'static>(&mut self, entity_id: usize) -> anyhow::Result<Option<B>> {
        fn remove_and_cons<B2: IsBundle + 'static>(
            index: &usize,
            arch: &mut Archetype,
        ) -> anyhow::Result<B2> {
            if let Some(root) = B2::as_root() {
                Ok(root)
            } else {
                let head = arch.get_column_mut::<B2::Head>()?.swap_remove(*index);
                let tail = remove_and_cons::<B2::Tail>(index, arch)?;
                Ok(B2::cons(head, tail))
            }
        }
        if let Some(index) = self.entity_lookup.remove(&entity_id) {
            assert_eq!(self.entities.swap_remove(index), entity_id);
            if index != self.entities.len() {
                // the index of the last entity was swapped for that of the removed index, so
                // we need to update the lookup
                self.entity_lookup.insert(self.entities[index], index);
            }
            Ok(Some(remove_and_cons(&index, self)?))
        } else {
            Ok(None)
        }
    }

    /// Remove the entity without knowledge of the bundle type and return its
    /// type-erased representation
    pub fn remove_any(&mut self, entity_id: usize) -> anyhow::Result<Option<AnyBundle>> {
        if let Some(index) = self.entity_lookup.remove(&entity_id) {
            assert_eq!(self.entities.swap_remove(index), entity_id);
            if index != self.entities.len() {
                // the index of the last entity was swapped for that of the removed index, so
                // we need to update the lookup
                self.entity_lookup.insert(self.entities[index], index);
            }
            let anybundle =
                self.data
                    .iter_mut()
                    .fold(AnyBundle::default(), |mut bundle, (ty, store)| {
                        let mut temp_store = store.clone_empty();
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

#[derive(Default)]
pub struct AllArchetypes {
    archetypes: Vec<Archetype>,
}

impl AllArchetypes {
    pub fn get_archetype<B: IsBundle + 'static>(&self) -> Option<&Archetype> {
        for archetype in self.archetypes.iter() {
            if archetype.matches_bundle::<B>().unwrap() {
                return Some(archetype);
            }
        }
        None
    }

    pub fn get_archetype_mut<B: IsBundle + 'static>(&mut self) -> Option<&mut Archetype> {
        for archetype in self.archetypes.iter_mut() {
            if archetype.matches_bundle::<B>().unwrap() {
                return Some(archetype);
            }
        }
        None
    }

    /// Inserts the bundle components for the given entity.
    ///
    /// ## Panics
    /// * if the bundle's types are not unique
    pub fn insert_bundle<B: IsBundle + 'static>(
        &mut self,
        entity_id: usize,
        bundle: B,
    ) -> Option<B> {
        let mut bundle_types: FxHashSet<TypeId> = B::type_info().unwrap();
        let mut any_bundle = bundle.into_any();
        let mut prev_bundle = None;

        // first find if the entity already exists
        for arch in self.archetypes.iter_mut() {
            if arch.contains_entity(&entity_id) {
                if arch.contains_bundle_components(&bundle_types) {
                    // we found a perfect match, simply store the bundle,
                    // returning the previous components
                    return Some(
                        B::try_from_any(arch.insert_any(entity_id, any_bundle).unwrap().unwrap())
                            .unwrap(),
                    );
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
        let new_arch = Archetype::try_from((entity_id, any_bundle)).unwrap();
        self.archetypes.push(new_arch);
        prev_bundle.and_then(|b| B::try_from_any(b).ok())
    }

    pub fn insert<T: 'static>(&mut self, entity_id: usize, component: T) -> Option<T> {
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
}

struct ArchetypeColumnInfo {
    // This column's archetype index in the AllArchetypes struct
    archetype_index: usize,
    // Extra info, aligned with the column
    entity_info: Arc<Vec<usize>>,
}

pub struct QueryResources {
    columns: FxHashMap<TypeId, Vec<(ArchetypeColumnInfo, Arc<AnyVec>)>>,
    mut_columns: FxHashMap<TypeId, Vec<(ArchetypeColumnInfo, AnyVec)>>,
}

pub struct QueryColumn<'a, T> {
    info: Vec<ArchetypeColumnInfo>,
    data: Vec<Arc<AnyVec>>,
    _phantom: PhantomData<&'a T>,
}

impl<'a, T> Iterator for QueryColumn<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

pub struct QueryColumnMut<'a, T> {
    info: Vec<ArchetypeColumnInfo>,
    data: Vec<AnyVec>,
    _phantom: PhantomData<&'a T>,
}

impl<'a, T: 'static> Iterator for QueryColumnMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

impl QueryResources {
    pub fn pop_column<C: 'static>(&mut self) -> Option<QueryColumn<C>> {
        let ty = TypeId::of::<C>();
        let (info, data): (Vec<ArchetypeColumnInfo>, Vec<Arc<AnyVec>>) =
            self.columns.remove(&ty)?.into_iter().unzip();
        Some(QueryColumn {
            info,
            data,
            _phantom: PhantomData,
        })
    }

    pub fn pop_column_mut<C: 'static>(&mut self) -> Option<QueryColumnMut<C>> {
        let ty = TypeId::of::<C>();
        let (info, data): (Vec<ArchetypeColumnInfo>, Vec<AnyVec>) =
            self.mut_columns.remove(&ty)?.into_iter().unzip();
        Some(QueryColumnMut {
            info,
            data,
            _phantom: PhantomData,
        })
    }
}

pub trait IsColumn {
    type Column: Iterator<Item = Self>;

    fn pop_column(resources: &mut QueryResources) -> Option<Self::Column>;
}

// impl<'a, T: 'static> IsColumn for &'a T {
//    type Column = QueryColumn<'a, T>;
//
//    fn pop_column(resources: &mut QueryResources) -> Option<Self::Column> {
//        todo!()
//    }
//
//}
// impl<'a, T: 'static> IsColumn for &'a mut T {
//    type Column = QueryColumnMut<'a, T>;
//
//    fn pop_column(resources: &mut QueryResources) -> Option<Self::Column> {
//        todo!()
//    }
//}

pub trait IsQueryItem<'a>: IsBundle<Head = Self::Column, Tail = Self::Rest> {
    type Column: IsColumn;
    type Rest: IsQueryItem<'a>;
    type Iter: Iterator<Item = Self> + 'a;

    fn iter_cons(
        col: <Self::Column as IsColumn>::Column,
        rest: <Self::Rest as IsQueryItem<'a>>::Iter,
    ) -> Self::Iter;

    fn resources_to_iter(
        mut resources: QueryResources,
    ) -> Option<Self::Iter> {
        let col: <Self::Column as IsColumn>::Column = <Self::Head as IsColumn>::pop_column(&mut resources)?;
        let tail: <Self::Rest as IsQueryItem>::Iter = <Self::Rest>::resources_to_iter(resources)?;
        Some(Self::iter_cons(col, tail))
        // let iter: _ = col
        //    .zip(tail)
        //    .map((|(head, rest)| Q::cons(head, rest)) as fn((Q::Head, Q::Tail)) ->
        // Q); Some(iter)
    }
}

#[cfg(test)]
mod test {
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
}
