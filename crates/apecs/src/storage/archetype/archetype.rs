//! Entity components stored together in contiguous arrays.
use std::{
    any::TypeId,
    ops::DerefMut,
    sync::Arc,
};

use any_vec::{any_value::AnyValueMut, traits::*, AnyVec};
use anyhow::Context;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::storage::Entry;

use super::bundle::*;

#[derive(Debug)]
pub struct InnerColumn(Arc<RwLock<AnyVec<dyn Sync + Send + 'static>>>);

/// A collection of entities having the same component types
#[derive(Debug, Default)]
pub struct Archetype {
    /// Types stored as Entry<_> in order, in data.
    pub(crate) entry_types: SmallVec<[TypeId; 4]>,
    pub(crate) data: SmallVec<[Arc<RwLock<AnyVec<dyn Send + Sync + 'static>>>; 4]>,
    // cache of entity_id to storage index
    pub(crate) entity_lookup: FxHashMap<usize, usize>,
    // cache of storage index to entity_id
    pub(crate) index_lookup: Vec<usize>,
}

#[derive(Default)]
pub struct ArchetypeBuilder {
    min_and_len: Option<(usize, usize)>,
    archetype: Archetype,
}

impl ArchetypeBuilder {
    pub fn with_components<T: Send + Sync + 'static>(
        mut self,
        starting_entity: usize,
        comps: impl ExactSizeIterator<Item = T>,
    ) -> Self {
        let ty = TypeId::of::<Entry<T>>();
        assert!(
            self.archetype
                .entry_types
                .iter()
                .find(|t| *t == &ty)
                .is_none(),
            "{} is already contained in the archetype",
            std::any::type_name::<T>()
        );

        let end = starting_entity + comps.len();
        let range = starting_entity..end;
        if let Some((min, len)) = self.min_and_len {
            assert_eq!(
                min, starting_entity,
                "starting entity id is not the same as the previous starting id"
            );
            assert_eq!(len, comps.len(), "length is not the same as previous");
        } else {
            self.min_and_len = Some((starting_entity, comps.len()));
            self.archetype.index_lookup = range.clone().collect();
            self.archetype.entity_lookup = range.clone().zip(0..).collect();
        }
        let anyvec = AnyVec::anyvec_from_iter(comps.zip(range).map(|(c, id)| Entry::new(id, c)));
        self.archetype.entry_types.push(ty);
        self.archetype.data.push(Arc::new(RwLock::new(anyvec)));
        self
    }

    pub fn build(mut self) -> Archetype {
        self.archetype.entry_types.sort();
        self.archetype.data.sort_by(|a, b| {
            a.read()
                .element_typeid()
                .cmp(&b.read().element_typeid())
        });
        self.archetype
    }
}

impl Archetype {
    /// Attempt to create a new `Archetype` from an `AnyBundle` of
    /// `Entry<_>`.
    pub fn try_from_any_entry_bundle(
        may_entity_id: Option<usize>,
        bundle: AnyBundle,
    ) -> anyhow::Result<Self> {
        let entry_types = bundle.0;
        let (index_lookup, entity_lookup) = if let Some(entity_id) = may_entity_id {
            (vec![entity_id], FxHashMap::from_iter([(entity_id, 0)]))
        } else {
            (vec![], FxHashMap::default())
        };
        Ok(Archetype {
            data: bundle
                .1
                .into_iter()
                .map(|v| Arc::new(RwLock::new(v)))
                .collect(),
            entity_lookup,
            index_lookup,
            entry_types,
        })
    }

    pub fn new<B: IsBundle>() -> anyhow::Result<Self> {
        let entry_bundle = <B::EntryBundle as IsBundle>::empty_any_bundle()?;
        Ok(Archetype {
            data: entry_bundle
                .1
                .into_iter()
                .map(|v| Arc::new(RwLock::new(v)))
                .collect(),
            entry_types: entry_bundle.0,
            entity_lookup: Default::default(),
            index_lookup: Default::default(),
        })
    }

    pub fn len(&self) -> usize {
        self.entity_lookup.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entity_lookup.is_empty()
    }

    /// Determine the index of the given `Entry<T>` `TypeId`.
    #[inline]
    pub fn index_of(&self, ty: &TypeId) -> Option<usize> {
        self.entry_types
            .iter()
            .enumerate()
            .find_map(|(i, t)| if ty == t { Some(i) } else { None })
    }

    /// Whether this archetype contains the entity
    pub fn contains_entity(&self, entity_id: &usize) -> bool {
        self.entity_lookup.contains_key(entity_id)
    }

    /// Whether this archetype contains `T` components
    pub fn has_component<T: 'static>(&self) -> bool {
        self.entry_types.contains(&TypeId::of::<Entry<T>>())
    }

    /// Whether this archetype's types match the given bundle type exactly.
    ///
    /// ## Errs
    /// Errs if the bundle contains duplicate types.
    pub fn matches_bundle<B: IsBundle>(&self) -> anyhow::Result<bool> {
        let types = <B::EntryBundle as IsBundle>::ordered_types()?;
        Ok(self.entry_types == types)
    }

    /// Whether this archetype's types contain all of the bundle's component
    /// types.
    ///
    /// # NOTE:
    /// `types` must be ordered, ascending
    pub fn contains_entry_types(&self, types: &[TypeId]) -> bool {
        let mut here = self.entry_types.iter();
        for ty in types.iter() {
            if here.find(|t| t == &ty).is_none() {
                return false;
            }
        }
        true
    }

    /// Attempts to get a clone of the column matching the given entry type.
    pub fn column_clone(&self, ty: &TypeId) -> Option<InnerColumn> {
        let i = self.index_of(ty)?;
        let data = self.data[i].clone();
        let col = InnerColumn(data);
        Some(col)
    }

    /// Insert a component bundle with the given entity id.
    ///
    /// ## Errs
    /// Errs if the bundle's types don't match the `Archetype`, or if the bundle
    /// contains duplicate types.
    ///
    /// ## WARNING
    /// Be certain of the types you are inserting. Because of Rust's coercion
    /// of types (or lack thereof), this function may fail unexpectedly when a
    /// type is not coerced as expected. A good example is with f32:
    ///
    /// ```rust,ignore
    /// let mut arch = Archetype::new::<(f32,)>().unwrap();
    /// arch.insert(0, (0.0,)).unwrap(); // panic!
    /// ```
    ///
    /// The above example panics because `0.0` by default is `f64`. The
    /// following works as expected:
    ///
    /// ```rust,ignore
    /// let mut arch = Archetype::new::<(f32,)>().unwrap();
    /// arch.insert(0, (0.0f32,)).unwrap();
    /// ```
    ///
    /// Or this way:
    /// ```rust,ignore
    /// let mut arch = Archetype::new::<(f32,)>().unwrap();
    /// arch.insert::<(f32,)>(0, (0.0,)).unwrap();
    /// ```
    pub fn insert_bundle<B>(&mut self, entity_id: usize, bundle: B) -> anyhow::Result<Option<B>>
    where
        B: IsBundle + 'static,
    {
        let bundle = bundle.into_entry_bundle(entity_id);
        Ok(
            if let Some(prev) =
                self.insert_any_entry_bundle(entity_id, bundle.try_into_any_bundle()?)?
            {
                let entry_bundle = <B::EntryBundle as IsBundle>::try_from_any_bundle(prev)?;
                let bundle = B::from_entry_bundle(entry_bundle);
                Some(bundle)
            } else {
                None
            },
        )
    }

    /// Insert an entry bundle into the archetype.
    ///
    /// Returns any previously stored bundle component entries.
    ///
    /// ## Errs
    /// Errs if the bundle types do not match the archetype.
    pub(crate) fn insert_any_entry_bundle(
        &mut self,
        entity_id: usize,
        mut bundle: AnyBundle,
    ) -> anyhow::Result<Option<AnyBundle>> {
        anyhow::ensure!(self.entry_types == bundle.0, "types don't match");
        Ok(
            if let Some(entity_index) = self.entity_lookup.get(&entity_id) {
                for (ty, component_vec) in bundle.0.iter().zip(bundle.1.iter_mut()) {
                    debug_assert_eq!(ty, &component_vec.element_typeid());
                    let ty_index = self.index_of(ty).context("missing column")?;
                    let mut data = self.data[ty_index].write();
                    let mut element_a = data.at_mut(*entity_index);
                    let mut element_b = component_vec.at_mut(0);
                    element_a.swap(element_b.deref_mut());
                }
                Some(bundle)
            } else {
                let last_index = self.entity_lookup.len();
                assert!(self.entity_lookup.insert(entity_id, last_index).is_none());
                self.index_lookup.push(entity_id);
                for (ty, mut component_vec) in bundle.0.into_iter().zip(bundle.1.into_iter()) {
                    debug_assert_eq!(ty, component_vec.element_typeid());
                    let ty_index = self
                        .index_of(&ty)
                        .with_context(|| format!("missing column: {:?}", ty))?;
                    let mut data = self.data[ty_index].write();
                    debug_assert_eq!(component_vec.element_typeid(), data.element_typeid());
                    data.push(component_vec.pop().unwrap());
                    debug_assert!(data.len() == last_index + 1);
                }
                None
            },
        )
    }

    /// Remove the entity and return its bundle, if any.
    pub fn remove_bundle<B: IsBundle>(&mut self, entity_id: usize) -> anyhow::Result<Option<B>> {
        Ok(
            if let Some(prev) = self.remove_any_entry_bundle(entity_id)? {
                let prev: B::EntryBundle = <B::EntryBundle as IsBundle>::try_from_any_bundle(prev)?;
                Some(B::from_entry_bundle(prev))
            } else {
                None
            },
        )
    }

    /// Remove the entity without knowledge of the bundle type and return its
    /// type-erased entries.
    pub(crate) fn remove_any_entry_bundle(
        &mut self,
        entity_id: usize,
    ) -> anyhow::Result<Option<AnyBundle>> {
        if let Some(index) = self.entity_lookup.remove(&entity_id) {
            // NOTE: last_index is the length of entity_lookup because we removed the entity
            // in question already!
            let last_index = self.entity_lookup.len();
            let mut should_reset_index = index != last_index;
            let mut anybundle = AnyBundle::default();
            anybundle.0 = self.entry_types.clone();
            assert_eq!(self.index_lookup.swap_remove(index), entity_id);
            for column in self.data.iter_mut() {
                let mut data = column.write();
                if should_reset_index {
                    // the index of the last entity was swapped for that of the removed
                    // index, so we need to update the lookup
                    self.entity_lookup.insert(self.index_lookup[index], index);
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

    // Visit a specific component with a closure.
    pub fn visit<T: 'static, A>(
        &self,
        entity_id: usize,
        f: impl FnOnce(&Entry<T>) -> A,
    ) -> anyhow::Result<Option<A>> {
        if let Some(entity_index) = self.entity_lookup.get(&entity_id) {
            let ty = TypeId::of::<Entry<T>>();
            if let Some(ty_index) = self.index_of(&ty) {
                let data = self.data[ty_index].read();
                let entries = data.downcast_ref::<Entry<T>>().context("can't downcast")?;
                Ok(entries.get(*entity_index).map(f))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    // Visit a specific mutable component with a closure.
    pub fn visit_mut<T: 'static, A>(
        &self,
        entity_id: usize,
        f: impl FnOnce(&mut Entry<T>) -> A,
    ) -> anyhow::Result<Option<A>> {
        if let Some(entity_index) = self.entity_lookup.get(&entity_id) {
            let ty = TypeId::of::<Entry<T>>();
            if let Some(ty_index) = self.index_of(&ty) {
                let mut data = self.data[ty_index].write();
                let mut entries = data.downcast_mut::<Entry<T>>().context("can't downcast")?;
                Ok(entries.get_mut(*entity_index).map(f))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod test {
    use any_vec::{mem::Heap, AnyVecMut, AnyVecRef};
    use std::ops::{Deref, DerefMut};

    use crate::storage::*;

    use super::*;

    #[test]
    fn new_archetype_has_ty() {
        let arch = Archetype::new::<(f32,)>().unwrap();
        assert_eq!(Some(0), arch.index_of(&TypeId::of::<Entry<f32>>()));
    }

    #[test]
    fn archetype_can_create_add_remove_get_mut() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut arch = Archetype::new::<(f32, &str, u32)>().unwrap();
        assert!(arch
            .insert_bundle(0, (0.0f32, "zero", 0u32))
            .unwrap()
            .is_none());
        assert!(arch
            .insert_bundle(1, (1.0f32, "one", 1u32))
            .unwrap()
            .is_none());
        assert!(arch
            .insert_bundle(2, (2.0f32, "two", 2u32))
            .unwrap()
            .is_none());
        assert!(arch
            .insert_bundle(3, (3.0f32, "three", 3u32))
            .unwrap()
            .is_none());
        assert_eq!(
            Some((1.0f32, "one", 1u32)),
            arch.insert_bundle(1, (111.0f32, "one", 1u32)).unwrap()
        );
        assert_eq!(
            (111.0f32, "one", 1u32),
            arch.remove_bundle(1).unwrap().unwrap()
        );
        assert_eq!(
            Some(3.0f32),
            arch.visit(3, |c: &Entry<f32>| *c.value()).unwrap(),
            "{:#?}",
            arch
        );
        arch.visit_mut(0, |c: &mut Entry<&str>| {
            c.set_value("000");
        })
        .unwrap();
        let actual: Option<String> = arch.visit::<&str, _>(0, |c| c.to_string()).unwrap();
        assert_eq!(Some("000".to_string()), actual, "{:#?}", arch.entity_lookup);
    }

    #[test]
    fn archetype_can_match() {
        let bundle = <(f32, bool)>::empty_any_bundle().unwrap();
        let arch = Archetype::new::<(f32, bool)>().unwrap();
        assert!(
            !arch.entry_types.is_empty(),
            "archetype types is empty: {:?}",
            bundle.0
        );
        fn assert_matches<B: IsBundle>(a: &Archetype) {
            match a.matches_bundle::<B>() {
                Ok(does) => assert!(
                    does,
                    "{:?} != {:?}",
                    B::ordered_types().unwrap(),
                    a.entry_types
                ),
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

    //#[test]
    // fn canfetch_query() {
    //    let _ = env_logger::builder()
    //        .is_test(true)
    //        .filter_level(log::LevelFilter::Trace)
    //        .try_init();

    //    let mut all = AllArchetypes::default();
    //    let _ = all.insert_bundle(0, (0.0f32, true));
    //    let _ = all.insert_bundle(1, (1.0f32, false));
    //    let _ = all.insert_bundle(2, (2.0f32, true));
    //    let mut world = World::default();
    //    world.with_resource(all).unwrap();

    //    {
    //        log::debug!("fetching query 1");
    //        let mut query = world.fetch::<Query<(&f32, &bool)>>().unwrap();
    //        log::debug!("using query 1.1");
    //        let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f,
    // *b)).unzip();        assert_eq!(vec![0.0, 1.0, 2.0], f32s);
    //        assert_eq!(vec![true, false, true], bools);
    //        // do it again
    //        log::debug!("using query 1.2");
    //        let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f,
    // *b)).unzip();        assert_eq!(vec![0.0, 1.0, 2.0], f32s);
    //        assert_eq!(vec![true, false, true], bools);
    //    }
    //    {
    //        log::debug!("fetching query 2");
    //        let mut query = world.fetch::<Query<(&mut f32, &mut
    // bool)>>().unwrap();        log::debug!("using query 2.1");
    //        for (mut f, mut b) in query.run() {
    //            *f *= 3.0;
    //            *b = *f < 5.0;
    //        }
    //        log::debug!("using query 2.2");
    //        let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f,
    // *b)).unzip();        assert_eq!(
    //            (vec![0.0, 3.0, 6.0], vec![true, true, false]),
    //            (f32s, bools)
    //        );
    //    }
    //    {
    //        log::debug!("fetching query 3");
    //        let mut query = world.fetch::<Query<(&f32, &bool)>>().unwrap();
    //        log::debug!("using query 3.1");
    //        let (f32s, bools): (Vec<_>, Vec<_>) = query.run().map(|(f, b)| (*f,
    // *b)).unzip();        assert_eq!(
    //            (vec![0.0, 3.0, 6.0], vec![true, true, false]),
    //            (f32s, bools)
    //        );
    //    }
    //}

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
}
