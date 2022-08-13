//! Operate on all archetypes.
use std::{any::TypeId, ops::DerefMut};

use any_vec::{any_value::AnyValueMut, AnyVec};

use crate::storage::Entry;

use super::{bundle::*, Archetype, InnerColumn};

#[derive(Debug)]
pub struct AllArchetypes {
    pub archetypes: Vec<Archetype>,
    // cache of entity_id to (archetype_index, component_index)
    pub(crate) entity_lookup: Vec<Option<(usize, usize)>>,
}

impl Default for AllArchetypes {
    fn default() -> Self {
        Self {
            archetypes: Default::default(),
            entity_lookup: Default::default(),
        }
    }
}

impl AllArchetypes {
    pub fn get_column<T: 'static>(&self) -> Vec<InnerColumn> {
        let ty = TypeId::of::<T>();
        self.archetypes
            .iter()
            .filter_map(|arch| arch.column_clone(&ty))
            .collect()
    }

    /// Attempts to obtain a mutable reference to an archetype that exactly
    /// matches the given types.
    ///
    /// ## NOTE:
    /// The types given must be ordered, ascending.
    pub fn get_archetype_mut(&mut self, types: &[TypeId]) -> Option<(usize, &mut Archetype)> {
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if arch.entry_types.as_slice() == types {
                return Some((i, arch));
            }
        }
        None
    }

    pub fn insert_archetype(&mut self, archetype: Archetype) {
        if let Some(_) = self.get_archetype_mut(&archetype.entry_types) {
            panic!("archetype already exists");
        } else {
            self.archetypes.push(archetype);
        }
    }

    /// Remove the entity without knowledge of the bundle type and return its
    /// type-erased entries.
    ///
    /// This also keeps the archetype's fields in sync.
    fn remove_any_entry_bundle(
        &mut self,
        entity_id: usize,
        archetype_index: usize,
        component_index: usize,
    ) -> AnyBundle {
        // do upkeep on our indicies to keep them all in sync with the removal
        assert_eq!(
            self.archetypes[archetype_index]
                .index_lookup
                .swap_remove(component_index),
            entity_id
        );
        let last_index = self.archetypes[archetype_index].index_lookup.len();
        if component_index != last_index {
            // the index of the last entity was swapped for that of the removed
            // index, so we need to update the lookup
            self.entity_lookup[self.archetypes[archetype_index].index_lookup[component_index]] =
                Some((archetype_index, component_index));
        }
        self.entity_lookup[entity_id] = None;

        // now do the actual removal
        let mut anybundle = AnyBundle::default();
        anybundle.0 = self.archetypes[archetype_index].entry_types.clone();
        for column in self.archetypes[archetype_index].data.iter_mut() {
            let mut data = column.write();

            let mut temp_store = data.clone_empty();
            debug_assert!(
                component_index < data.len(),
                "store len {} does not contain index {}",
                data.len(),
                component_index
            );
            let value = data.swap_remove(component_index);
            temp_store.push(value);
            anybundle.1.push(temp_store);
        }
        anybundle
    }

    fn swap_any_entry_bundle_unchecked(
        &mut self,
        archetype_index: usize,
        component_index: usize,
        entry_any_bundle: &mut AnyBundle,
    ) {
        for (components, component_any) in self.archetypes[archetype_index]
            .data
            .iter_mut()
            .zip(entry_any_bundle.1.iter_mut())
        {
            let components: &mut AnyVec<_> = &mut components.write();
            let mut existing_component = components.get_mut(component_index).unwrap();
            let mut new_component = component_any.get_mut(0).unwrap();
            existing_component.swap(new_component.deref_mut());
        }
    }

    fn append_new_entry_bundle(&mut self, entity_id: usize, entry_bundle: AnyBundle) {
        if let Some((i, arch)) = self.get_archetype_mut(&entry_bundle.0) {
            for (components, mut component_any) in
                arch.data.iter_mut().zip(entry_bundle.1.into_iter())
            {
                let components = &mut components.write();
                components.push(component_any.pop().unwrap());
            }
            let path = (i, arch.index_lookup.len());
            arch.index_lookup.push(entity_id);
            if entity_id >= self.entity_lookup.len() {
                self.entity_lookup
                    .resize_with(entity_id + 1, Default::default);
            }
            self.entity_lookup[entity_id] = Some(path);
        } else {
            // there is no archetype to store it in, so we create a new one
            let archetype_index = self.archetypes.len();
            let component_index = 0;
            if entity_id >= self.entity_lookup.len() {
                self.entity_lookup
                    .resize_with(entity_id + 1, Default::default);
            }
            self.entity_lookup[entity_id] = Some((archetype_index, component_index));
            let new_arch =
                Archetype::try_from_any_entry_bundle(Some(entity_id), entry_bundle).unwrap();
            self.archetypes.push(new_arch);
        }
    }

    /// Inserts the bundle components for the given entity.
    ///
    /// ## Panics
    /// * if the bundle's types are not unique
    pub fn insert_bundle<B: IsBundle>(&mut self, entity_id: usize, bundle: B) {
        // determine if the entity already exists
        if let Some((archetype_index, component_index)) = self
            .entity_lookup
            .get(entity_id)
            .map(Option::as_ref)
            .flatten()
        {
            let mut entry_any_bundle: AnyBundle = bundle
                .into_entry_bundle(entity_id)
                .try_into_any_bundle()
                .unwrap();
            if entry_any_bundle.0 == self.archetypes[*archetype_index].entry_types {
                // the types are the same, so we only need to swap components
                self.swap_any_entry_bundle_unchecked(
                    *archetype_index,
                    *component_index,
                    &mut entry_any_bundle,
                );
            } else {
                // the types are not the same, so we have to combine them and then find a new
                // archetype to store it in
                let mut previous_bundle =
                    self.remove_any_entry_bundle(entity_id, *archetype_index, *component_index);
                for (ty, any) in entry_any_bundle
                    .0
                    .into_iter()
                    .zip(entry_any_bundle.1.into_iter())
                {
                    previous_bundle.insert(ty, any);
                }
                self.append_new_entry_bundle(entity_id, previous_bundle);
            }
        } else {
            let entry_bundle: AnyBundle = bundle
                .into_entry_bundle(entity_id)
                .try_into_any_bundle()
                .unwrap();
            self.append_new_entry_bundle(entity_id, entry_bundle);
        }
    }

    /// Insert a single component.
    ///
    /// Returns the previous component, if possible.
    pub fn insert_component<T: Send + Sync + 'static>(
        &mut self,
        entity_id: usize,
        mut component: T,
    ) -> Option<T> {
        // determine if the entity already exists
        if let Some((archetype_index, component_index)) = self
            .entity_lookup
            .get(entity_id)
            .map(Option::as_ref)
            .flatten()
        {
            let ty = TypeId::of::<Entry<T>>();
            if let Some(ty_index) = self.archetypes[*archetype_index].index_of(&ty) {
                // everything matches, just swap
                let mut col = self.archetypes[*archetype_index].data[ty_index].write();
                let entry = col
                    .get_mut(*component_index)
                    .unwrap()
                    .downcast_mut::<Entry<T>>()
                    .unwrap();
                std::mem::swap(entry.deref_mut(), &mut component);
                return Some(component);
            } else {
                // the types are not the same, so we have to combine them and then find a new
                // archetype to store it in
                let mut previous_bundle =
                    self.remove_any_entry_bundle(entity_id, *archetype_index, *component_index);
                previous_bundle.insert(ty, AnyVec::wrap(Entry::new(entity_id, component)));
                self.append_new_entry_bundle(entity_id, previous_bundle);
            }
        } else {
            let entry_bundle: AnyBundle = (component,)
                .into_entry_bundle(entity_id)
                .try_into_any_bundle()
                .unwrap();
            self.append_new_entry_bundle(entity_id, entry_bundle);
        }
        None
    }

    /// Remove all components of the given entity and return them as a typed
    /// bundle.
    ///
    /// ## Warning
    /// Any components not contained in `B` will be discarded.
    ///
    /// ## Panics
    /// Panics if the component types are not unique.
    pub fn remove<B: IsBundle>(&mut self, entity_id: usize) -> Option<B> {
        self.entity_lookup.get(entity_id).copied().flatten().map(
            |(archetype_index, component_index)| {
                let any_entry_bundle =
                    self.remove_any_entry_bundle(entity_id, archetype_index, component_index);
                let entry_bundle =
                    <B::EntryBundle as IsBundle>::try_from_any_bundle(any_entry_bundle).unwrap();
                B::from_entry_bundle(entry_bundle)
            },
        )
    }

    /// Remove a single component from the given entity.
    pub fn remove_component<T: Send + Sync + 'static>(&mut self, entity_id: usize) -> Option<T> {
        let (archetype_index, component_index) = self
            .entity_lookup
            .get(entity_id)
            .map(Option::as_ref)
            .flatten()?;
        let ty = TypeId::of::<Entry<T>>();
        if self.archetypes[*archetype_index].contains_entry_types(&[ty]) {
            let mut entry_bundle =
                self.remove_any_entry_bundle(entity_id, *archetype_index, *component_index);
            let t: T = entry_bundle.remove::<Entry<T>>(&ty).ok()?.into_inner();
            self.append_new_entry_bundle(entity_id, entry_bundle);
            Some(t)
        } else {
            None
        }
    }

    /// Return the number of entities with archetypes.
    pub fn len(&self) -> usize {
        self.archetypes.iter().map(|a| a.len()).sum()
    }

    /// Return whether any entities with archetypes exist in storage.
    pub fn is_empty(&self) -> bool {
        self.archetypes.iter().all(|a| a.is_empty())
    }

    /// Perform upkeep on all archetypes, removing any given dead ids.
    pub fn upkeep(&mut self, dead_ids: &[usize]) {
        for id in dead_ids {
            if let Some((archetype_index, component_index)) = self.entity_lookup.get(*id).copied().flatten() {
                log::trace!(
                    "removing entity {} from archetype {}:{}",
                    id,
                    archetype_index,
                    component_index
                );
                let _ = self.remove_any_entry_bundle(*id, archetype_index, component_index);
                if self.archetypes[archetype_index].index_lookup.is_empty() {
                    log::trace!("archetype {} is empty", archetype_index);
                    // remove the archetype
                    self.archetypes.swap_remove(archetype_index);
                    // the last index has swapped to this index, so we must update our lookups
                    let last_index = self.archetypes.len();
                    if archetype_index != last_index {
                        for entity_id in
                            self.archetypes[archetype_index].index_lookup.clone().iter()
                        {
                            let (prev_archetype_index, _) = self
                                .entity_lookup
                                .get_mut(*entity_id)
                                .map(Option::as_mut)
                                .flatten()
                                .unwrap();
                            *prev_archetype_index = archetype_index;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use crate::storage::archetype::AllArchetypes;
    #[test]
    fn all_archetypes_send_sync() {
        let _: Box<dyn Send + Sync + 'static> = Box::new(AllArchetypes::default());
    }

    #[test]
    fn all_archetypes_can_create_add_remove_get_mut() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut arch = AllArchetypes::default();
        assert!(arch.insert_component(0, 0.0f32).is_none());
        assert!(arch.insert_component(1, 1.0f32).is_none());
        assert!(arch.insert_component(2, 2.0f32).is_none());
        assert!(arch.insert_component(3, 3.0f32).is_none());
        assert!(arch.insert_component(1, "one".to_string()).is_none());
        assert!(arch.insert_component(2, "two".to_string()).is_none());
        assert_eq!(arch.insert_component(3, 3.33f32).unwrap(), 3.00);

        let mut arch = AllArchetypes::default();
        arch.insert_bundle(0, (0.0f32, "zero", 0u32));
        arch.insert_bundle(1, (1.0f32, "one", 1u32));
        arch.insert_bundle(2, (2.0f32, "two", 2u32));
        arch.insert_bundle(3, (3.0f32, "three", 3u32));
        arch.insert_bundle(1, (111.0f32, "one", 1u32));
        assert_eq!((111.0f32, "one", 1u32), arch.remove(1).unwrap());
    }

    #[test]
    fn all_archetypes_can_remove() {
        let mut all = AllArchetypes::default();

        for id in 0..10000 {
            all.insert_component(id, id);
        }

        {
            // verify our inserts
            let mut q = all.query::<&usize>();
            assert_eq!(
                (0..10000).map(|i| (i, i)).collect::<Vec<_>>(),
                q.iter_mut().map(|i| (i.id(), **i)).collect::<Vec<_>>()
            );
        }

        for id in 0..10000 {
            all.insert_component::<f32>(id, id as f32);
        }

        {
            // verify our inserts
            let mut q = all.query::<(&usize, &f32)>();
            assert_eq!(
                (0..10000).map(|i| (i, i, i as f32)).collect::<Vec<_>>(),
                q.iter_mut()
                    .map(|(i, f)| (i.id(), **i, **f))
                    .collect::<Vec<_>>()
            );
        }

        for id in 0..10000 {
            let _ = all.remove::<(f32,)>(id);
        }
        assert!(all.is_empty());
    }

    //#[test]
    // fn all_archetypes_add_remove_component() {
    //    struct A(f32);
    //    struct B(f32);

    //    let n = 10_000;
    //    let mut all = AllArchetypes::default();
    //    let archetype = ArchetypeBuilder::default()
    //        .with_components(0, (0..n).map(|_| A(0.0)))
    //        .build();
    //    all.insert_archetype(archetype);

    //    for id in 0..n {
    //        let _ = all.insert_component(id, B(0.0));
    //    }

    //    for id in 0..n {
    //        let _ = all.remove_component::<B>(id);
    //    }
    //}
}
