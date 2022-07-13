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
    AnyVec, AnyVecMut, AnyVecRef, AnyVecTyped,
};
use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

mod bundle;
pub use bundle::*;
use tuple_list::TupleList;

use crate::schedule::IsBatch;

/// A collection of entities having the same component types
#[derive(Default)]
pub struct Archetype {
    pub(crate) types: FxHashSet<TypeId>,
    pub(crate) entities: Vec<usize>,
    // map of entity_id to storage index
    pub(crate) entity_lookup: FxHashMap<usize, usize>,
    pub(crate) data: FxHashMap<TypeId, AnyVec>,
}

impl TryFrom<()> for Archetype {
    type Error = anyhow::Error;

    fn try_from(_: ()) -> anyhow::Result<Self> {
        Ok(Archetype::default())
    }
}

impl<Head, Tail> TryFrom<(Head, Tail)> for Archetype
where
    Head: 'static,
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
        let mut anyvec: AnyVec = AnyVec::new::<Head>();
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

    pub fn take_column(&mut self, ty: &TypeId) -> Option<AnyVec> {
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
    // pub fn get_archetype<B: IsBundleTypeInfo + 'static>(&self) ->
    // Option<&Archetype> {    for archetype in self.archetypes.iter() {
    //        if archetype.matches_bundle::<B>().unwrap() {
    //            return Some(archetype);
    //        }
    //    }
    //    None
    //}

    // pub fn get_archetype_mut<B: Tuple + 'static>(&mut self) -> Option<&mut
    // Archetype> {    for archetype in self.archetypes.iter_mut() {
    //        if archetype.matches_bundle::<B>().unwrap() {
    //            return Some(archetype);
    //        }
    //    }
    //    None
    //}

    pub fn get_column<T: 'static>(&mut self) -> Option<QueryColumn<T>> {
        let ty = TypeId::of::<T>();
        let mut column = QueryColumn::<T>::default();
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if let Some(col) = arch.take_column(&ty) {
                column.info.push(ArchetypeColumnInfo {
                    archetype_index: i,
                    entity_info: Arc::new(arch.entities.clone()),
                });
                column.data.push(Arc::new(col));
            }
        }
        if column.data.is_empty() {
            None
        } else {
            Some(column)
        }
    }

    pub fn get_column_mut<T: 'static>(&mut self) -> Option<QueryColumnMut<T>> {
        let ty = TypeId::of::<T>();
        let mut column = QueryColumnMut::<T>::default();
        for (i, arch) in self.archetypes.iter_mut().enumerate() {
            if let Some(col) = arch.take_column(&ty) {
                column.info.push(ArchetypeColumnInfo {
                    archetype_index: i,
                    entity_info: Arc::new(arch.entities.clone()),
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

pub struct QueryColumn<T> {
    info: Vec<ArchetypeColumnInfo>,
    data: Vec<Arc<AnyVec>>,
    _phantom: PhantomData<T>,
}

impl<T> Default for QueryColumn<T> {
    fn default() -> Self {
        Self {
            info: Default::default(),
            data: Default::default(),
            _phantom: Default::default(),
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

pub struct QueryColumnMut<T> {
    info: Vec<ArchetypeColumnInfo>,
    data: Vec<AnyVec>,
    _phantom: PhantomData<T>,
}

impl<T> Default for QueryColumnMut<T> {
    fn default() -> Self {
        Self {
            info: Default::default(),
            data: Default::default(),
            _phantom: Default::default(),
        }
    }
}

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

    // fn columns_iter(columns: &mut Self::Columns) -> Self::Iter;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Option<Self::Columns>;

    fn columns_iter(columns: &mut Self::Columns) -> Self::ColumnsIter<'_>;
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
}

impl<'a, T: 'static> IsQuery for &'a T {
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
}

impl<'a, T: 'static> IsQuery for &'a mut T {
    type Columns = QueryColumnMut<T>;
    type Item<'t> = &'t mut T;
    type ColumnsIter<'t> = QueryColumnIterMut<'t, T>;

    fn pop_columns(archetypes: &mut AllArchetypes) -> Option<Self::Columns> {
        Some(archetypes.get_column_mut::<T>()?)
    }

    fn columns_iter(cols: &mut Self::Columns) -> Self::ColumnsIter<'_> {
        let mut vs = cols
            .data
            .iter_mut()
            .map(|v| v.downcast_mut().unwrap())
            .collect::<Vec<_>>();
        let first = vs.pop().map(|mut v| v.iter_mut());
        QueryColumnIterMut(first, vs)
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
}

pub struct Query<T>(<T::TupleList as IsQuery>::Columns)
where
    T: Tuple + ?Sized,
    T::TupleList: IsQuery;

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

// impl QueryResources {
//    pub fn pop_column<C: 'static>(&mut self) -> Option<QueryColumn<'static,
// C>> {        let ty = TypeId::of::<C>();
//        let (info, data): (Vec<ArchetypeColumnInfo>, Vec<Arc<AnyVec>>) =
//            self.columns.remove(&ty)?.into_iter().unzip();
//        Some(QueryColumn {
//            info,
//            data,
//            _phantom: PhantomData,
//        })
//    }
//
//    pub fn pop_column_mut<C: 'static>(&mut self) ->
// Option<QueryColumnMut<'static, C>> {        let ty = TypeId::of::<C>();
//        let (info, data): (Vec<ArchetypeColumnInfo>, Vec<AnyVec>) =
//            self.mut_columns.remove(&ty)?.into_iter().unzip();
//        Some(QueryColumnMut {
//            info,
//            data,
//            _phantom: PhantomData,
//        })
//    }
//}

// pub trait IsColumn {
//    type Column: Iterator<Item = Self>;
//
//    fn pop_column(resources: &mut QueryResources) -> Option<Self::Column>;
//}
// impl<'a, T: 'static> IsColumn for &'a T {
//    type Column = QueryColumn<'a, T>;
//
//    fn pop_column(resources: &mut QueryResources) -> Option<Self::Column> {
//        resources.pop_column()
//    }
//
//}
// impl<'a, T: 'static> IsColumn for &'a mut T {
//    type Column = QueryColumnMut<'a, T>;
//
//    fn pop_column(resources: &mut QueryResources) -> Option<Self::Column> {
//        resources.pop_column_mut()
//    }
//}
// pub trait IsQueryItem {
//    type Iter: Iterator<Item = Self>;
//
//    fn resources_to_iter(resources: QueryResources) -> Option<Self::Iter>;
//}
// impl<A> IsQueryItem for (A,)
// where
//    A: IsColumn,
//{
//    type Iter = std::iter::Map<A::Column, fn(A) -> (A,)>;
//
//    fn resources_to_iter(mut resources: QueryResources) -> Option<Self::Iter>
// {        Some(A::pop_column(&mut resources)?.map(|a| (a,)))
//    }
//}
// impl<A, B> IsQueryItem for (A, B)
// where
//    A: IsColumn,
//    B: IsColumn,
//{
//    type Iter = std::iter::Zip<A::Column, B::Column>;
//
//    fn resources_to_iter(mut resources: QueryResources) -> Option<Self::Iter>
// {        Some(A::pop_column(&mut resources)?.zip(B::pop_column(&mut
// resources)?))    }
//}
// impl<A, B, C> IsQueryItem for (A, B, C)
// where
//    A: IsColumn,
//    B: IsColumn,
//    C: IsColumn,
//{
//    type Iter = std::iter::Map<
//        std::iter::Zip<std::iter::Zip<A::Column, B::Column>, C::Column>,
//        fn(((A, B), C)) -> (A, B, C),
//    >;
//
//    fn resources_to_iter(mut resources: QueryResources) -> Option<Self::Iter>
// {        Some(
//            A::pop_column(&mut resources)?
//                .zip(B::pop_column(&mut resources)?)
//                .zip(C::pop_column(&mut resources)?)
//                .map(|((a, b), c)| (a, b, c)),
//        )
//    }
//}

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
            let mut query: Query<(&String, &mut bool, &mut f32)> = Query::try_from(&mut store).unwrap();
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
}
