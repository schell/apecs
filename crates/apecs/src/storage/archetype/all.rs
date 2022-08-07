//! Operate on all archetypes.
use std::{any::TypeId, ops::DerefMut};

use any_vec::AnyVec;
use smallvec::SmallVec;

use crate::storage::Entry;

use super::{bundle::*, Archetype, InnerColumn};

#[derive(Debug)]
pub struct AllArchetypes {
    pub archetypes: Vec<Archetype>,
}

impl Default for AllArchetypes {
    fn default() -> Self {
        Self {
            archetypes: Default::default(),
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
    pub fn get_archetype_mut(&mut self, types: &[TypeId]) -> Option<&mut Archetype> {
        for arch in self.archetypes.iter_mut() {
            if arch.entry_types.as_slice() == types {
                return Some(arch);
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

    /// Inserts the bundle components for the given entity.
    ///
    /// ## Panics
    /// * if the bundle's types are not unique
    pub fn insert_bundle<B: IsBundle>(&mut self, entity_id: usize, bundle: B) -> Option<B> {
        let mut entry_bundle_types: SmallVec<[TypeId; 4]> =
            <B::EntryBundle as IsBundle>::ordered_types().unwrap();
        // The bundle we intend to store. This bundle's types may not match
        // output_bundle after the first step, because we may have found an
        // existing bundle at that entity and now have to store the union of the
        // two in a new archetype
        let mut input_any_entry_bundle = bundle
            .into_entry_bundle(entity_id)
            .try_into_any_bundle()
            .unwrap();
        // The output bundle we indend to give back to the caller
        let mut output_any_entry_bundle: Option<AnyBundle> = None;

        // first find if the entity already exists
        for arch in self.archetypes.iter_mut() {
            if arch.contains_entity(&entity_id) {
                if arch.contains_entry_types(&entry_bundle_types) {
                    // we found a perfect match, simply store the bundle,
                    // returning the previous components
                    let prev_any_entry_bundle = arch
                        .insert_any_entry_bundle(entity_id, input_any_entry_bundle)
                        .unwrap()
                        .unwrap();
                    let prev_entry_bundle =
                        <B::EntryBundle as IsBundle>::try_from_any_bundle(prev_any_entry_bundle)
                            .unwrap();
                    let prev_bundle = B::from_entry_bundle(prev_entry_bundle);
                    return Some(prev_bundle);
                }
                // otherwise we remove the bundle from the archetype, adding the
                // non-overlapping key+values to our bundle
                let mut old_bundle = arch.remove_any_entry_bundle(entity_id).unwrap().unwrap();
                output_any_entry_bundle = old_bundle.union(input_any_entry_bundle);
                input_any_entry_bundle = old_bundle;
                entry_bundle_types = input_any_entry_bundle.0.clone();
                break;
            }
        }

        // now search for an archetype to store the bundle
        for arch in self.archetypes.iter_mut() {
            if entry_bundle_types == arch.entry_types {
                // we found an archetype that will store our bundle, so store it
                let no_previous_bundle = arch
                    .insert_any_entry_bundle(entity_id, input_any_entry_bundle)
                    .unwrap()
                    .is_none();
                assert!(
                    no_previous_bundle,
                    "the archetype had a bundle at the entity"
                );
                return if let Some(any_entry_bundle) = output_any_entry_bundle {
                    let entry_bundle =
                        <B::EntryBundle as IsBundle>::try_from_any_bundle(any_entry_bundle)
                            .unwrap();
                    Some(B::from_entry_bundle(entry_bundle))
                } else {
                    None
                };
            }
        }

        // we couldn't find any existing archetypes that can hold the bundle, so make a
        // new one
        let new_arch =
            Archetype::try_from_any_entry_bundle(Some(entity_id), input_any_entry_bundle).unwrap();
        self.archetypes.push(new_arch);
        output_any_entry_bundle.and_then(|b| b.try_into_tuple().ok())
    }

    /// Insert a single component.
    ///
    /// Returns the previous component, if possible.
    pub fn insert_component<T: Send + Sync + 'static>(
        &mut self,
        entity_id: usize,
        mut component: T,
    ) -> Option<T> {
        let ty = TypeId::of::<Entry<T>>();
        let mut new_entry_bundle: Option<AnyBundle> = None;

        for arch in self.archetypes.iter_mut() {
            match (arch.index_of(&ty), arch.entity_lookup.get(&entity_id)) {
                (None, Some(_)) => {
                    // we have to remove, repack w/ new comp and then store in a new archetype
                    new_entry_bundle =
                        Some(arch.remove_any_entry_bundle(entity_id).unwrap().unwrap());
                    break;
                }
                (Some(ty_ndx), Some(ent_ndx)) => {
                    let mut data = arch.data[ty_ndx].write();
                    let mut entries = data.downcast_mut::<Entry<T>>().expect("can't downcast");
                    std::mem::swap(
                        entries.get_mut(*ent_ndx).unwrap().deref_mut(),
                        &mut component,
                    );
                    return Some(component);
                }
                _ => {}
            }
        }

        let entry_bundle = if let Some(mut bundle) = new_entry_bundle {
            bundle.insert(ty, AnyVec::wrap(Entry::new(entity_id, component)));
            bundle
        } else {
            (component,)
                .into_entry_bundle(entity_id)
                .try_into_any_bundle()
                .unwrap()
        };
        let entry_bundle_types = entry_bundle.type_info();
        // now search for an archetype to store the bundle
        for arch in self.archetypes.iter_mut() {
            if arch.entry_types.as_slice() == entry_bundle_types {
                // we found the archetype that can store our bundle, so store it
                let no_previous_bundle = arch
                    .insert_any_entry_bundle(entity_id, entry_bundle)
                    .unwrap()
                    .is_none();
                assert!(
                    no_previous_bundle,
                    "the archetype had a bundle at the entity"
                );
                return None;
            }
        }

        // we couldn't find any existing archetypes that can hold the bundle, so make a
        // new one
        let new_arch = Archetype::try_from_any_entry_bundle(Some(entity_id), entry_bundle).unwrap();
        self.archetypes.push(new_arch);
        None
    }

    // Visit a specific component with a closure.
    pub fn visit<T: 'static, A>(
        &self,
        entity_id: usize,
        f: impl FnOnce(&Entry<T>) -> A,
    ) -> Option<A> {
        for arch in self.archetypes.iter() {
            if arch.has_component::<T>() && arch.contains_entity(&entity_id) {
                return arch.visit::<T, A>(entity_id, f).unwrap();
            }
        }
        None
    }

    // Visit a specific mutable component with a closure.
    pub fn visit_mut<T: 'static, A>(
        &self,
        entity_id: usize,
        f: impl FnOnce(&mut Entry<T>) -> A,
    ) -> Option<A> {
        for arch in self.archetypes.iter() {
            if arch.has_component::<T>() && arch.contains_entity(&entity_id) {
                return arch.visit_mut::<T, A>(entity_id, f).unwrap();
            }
        }
        None
    }

    /// Remove all component entries of the given entity and return them in an
    /// untyped bundle.
    pub fn remove_any_entry_bundle(&mut self, entity_id: usize) -> Option<AnyBundle> {
        for arch in self.archetypes.iter_mut() {
            if let Some(bundle) = arch.remove_any_entry_bundle(entity_id).unwrap() {
                return Some(bundle);
            }
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
    pub fn remove_bundle<B: IsBundle>(&mut self, entity_id: usize) -> Option<B> {
        let any_entry_bundle = self.remove_any_entry_bundle(entity_id)?;
        let entry_bundle =
            <B::EntryBundle as IsBundle>::try_from_any_bundle(any_entry_bundle).unwrap();
        let bundle = B::from_entry_bundle(entry_bundle);
        Some(bundle)
    }

    /// Remove a single component from the given entity.
    pub fn remove_component<T: Send + Sync + 'static>(&mut self, entity_id: usize) -> Option<T> {
        #[inline]
        fn only_remove<S: Send + Sync + 'static>(
            id: usize,
            all: &mut AllArchetypes,
        ) -> (Option<AnyBundle>, Option<Entry<S>>) {
            for arch in all.archetypes.iter_mut() {
                if let Some(mut bundle) = arch.remove_any_entry_bundle(id).unwrap() {
                    let ty = TypeId::of::<Entry<S>>();
                    let prev: Option<Entry<S>> = bundle.remove::<Entry<S>>(&ty).ok();
                    return (Some(bundle), prev);
                }
            }
            (None, None)
        }
        let (entry_bundle, prev_entry) = only_remove(entity_id, self);

        let prev = prev_entry.map(|e: Entry<T>| e.into_inner());
        if let Some(entry_bundle) = entry_bundle {
            let entry_bundle_types = entry_bundle.type_info();
            // now search for an archetype to store the bundle
            for arch in self.archetypes.iter_mut() {
                if arch.entry_types.as_slice() == entry_bundle_types {
                    // we found the archetype that can store our bundle, so store it
                    let no_previous_bundle = arch
                        .insert_any_entry_bundle(entity_id, entry_bundle)
                        .unwrap()
                        .is_none();
                    assert!(
                        no_previous_bundle,
                        "the archetype had a bundle at the entity"
                    );
                    return prev;
                }
            }

            // we couldn't find any existing archetypes that can hold the bundle, so make a
            // new one
            let new_arch =
                Archetype::try_from_any_entry_bundle(Some(entity_id), entry_bundle).unwrap();
            self.archetypes.push(new_arch);
        }
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

#[cfg(test)]
mod test {
    use crate::storage::archetype::{AllArchetypes, ArchetypeBuilder};
    #[test]
    fn all_archetypes_send_sync() {
        let _: Box<dyn Send + Sync + 'static> = Box::new(AllArchetypes::default());
    }

    #[test]
    fn all_archetypes_can_create_add_remove_get_mut() {
        let mut arch = AllArchetypes::default();
        assert!(arch.insert_component(0, 0.0f32).is_none());
        assert!(arch.insert_component(1, 1.0f32).is_none());
        assert!(arch.insert_component(2, 2.0f32).is_none());
        assert!(arch.insert_component(3, 3.0f32).is_none());
        assert!(arch.insert_component(1, "one".to_string()).is_none());
        assert!(arch.insert_component(2, "two".to_string()).is_none());
        assert_eq!(arch.insert_component(3, 3.33f32).unwrap(), 3.00);
    }

    #[test]
    fn all_archetypes_can_remove() {
        let mut all = AllArchetypes::default();

        for id in 0..10000 {
            all.insert_component(id, id);
        }
        for id in 0..10000 {
            all.insert_component(id, id as f32);
        }
        for id in 0..10000 {
            let _ = all.remove_any_entry_bundle(id);
        }
        assert!(all.is_empty());
    }

    #[test]
    fn all_archetypes_add_remove_component() {
        struct A(f32);
        struct B(f32);

        let n = 10_000;
        let mut all = AllArchetypes::default();
        let archetype = ArchetypeBuilder::default()
            .with_components(0, (0..n).map(|_| A(0.0)))
            .build();
        all.insert_archetype(archetype);

        for id in 0..n {
            let _ = all.insert_component(id, B(0.0));
        }

        for id in 0..n {
            let _ = all.remove_component::<B>(id);
        }
    }
}
