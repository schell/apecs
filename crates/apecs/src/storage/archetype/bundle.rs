//! Bundle trait for decomposing component tuples.
use std::any::TypeId;

use any_vec::AnyVec;
use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};
use tuple_list::TupleList;

pub use tuple_list::Tuple;

pub trait TryFromAnyBundle: Sized + 'static {
    fn try_from_any(bundle: AnyBundle) -> anyhow::Result<Self>;
}

impl TryFromAnyBundle for () {
    fn try_from_any(_: AnyBundle) -> anyhow::Result<Self> {
        Ok(())
    }
}

impl<Head, Tail> TryFromAnyBundle for (Head, Tail)
where
    Head: 'static,
    Tail: TryFromAnyBundle + TupleList,
{
    fn try_from_any(mut bundle: AnyBundle) -> anyhow::Result<Self> {
        let ty = TypeId::of::<Head>();
        let mut any_head_vec = bundle.0.remove(&ty).with_context(|| {
            format!("bundle is missing type: {}", std::any::type_name::<Head>())
        })?;
        let mut head_vec = any_head_vec
            .downcast_mut::<Head>()
            .context("could not downcast")?;
        let head = head_vec
            .pop()
            .with_context(|| format!("missing '{}' component", std::any::type_name::<Head>()))?;

        Ok((head, Tail::try_from_any(bundle)?))
    }
}

pub trait TupleListTypeVec {
    fn tuple_list_type_vec() -> Vec<TypeId>;
}

impl TupleListTypeVec for () {
    fn tuple_list_type_vec() -> Vec<TypeId> {
        vec![]
    }
}

impl<Head, Tail> TupleListTypeVec for (Head, Tail)
where
    Head: 'static,
    Tail: TupleListTypeVec + TupleList,
{
    fn tuple_list_type_vec() -> Vec<TypeId> {
        let mut tys = Tail::tuple_list_type_vec();
        tys.push(TypeId::of::<Head>());
        tys
    }
}

pub trait IsBundleTypeInfo: Tuple {
    fn get_type_info() -> anyhow::Result<FxHashSet<TypeId>>;
}

impl<T> IsBundleTypeInfo for T
where
    T: Tuple,
    T::TupleList: TupleListTypeVec,
{
    fn get_type_info() -> anyhow::Result<FxHashSet<TypeId>> {
        let mut types = T::TupleList::tuple_list_type_vec();
        types.sort();
        ensure_type_info(&types)?;
        Ok(FxHashSet::from_iter(types.into_iter()))
    }
}

pub fn type_info<T: IsBundleTypeInfo>() -> anyhow::Result<FxHashSet<TypeId>> {
    T::get_type_info()
}

pub fn ensure_type_info(types: &[TypeId]) -> anyhow::Result<()> {
    for x in types.windows(2) {
        match x[0].cmp(&x[1]) {
            core::cmp::Ordering::Less => (),
            core::cmp::Ordering::Equal => {
                anyhow::bail!(
                    "attempted to allocate entity with duplicate components; each type must occur \
                     at most once!"
                )
            }
            core::cmp::Ordering::Greater => anyhow::bail!("type info is unsorted"),
        }
    }
    Ok(())
}

/// A type erased bundle of components.
#[derive(Default)]
pub struct AnyBundle(pub FxHashMap<TypeId, AnyVec>);

impl AnyBundle {
    /// Merge the given bundle into `self`, returning any key+values in
    /// `self` that match `other`.
    pub fn union(&mut self, other: AnyBundle) -> Option<Self> {
        let intersecting_types: FxHashSet<TypeId> = self
            .type_info()
            .intersection(&other.type_info())
            .map(Clone::clone)
            .collect();
        let types_overlap = !intersecting_types.is_empty();
        let prev = if types_overlap {
            let mut intersection = AnyBundle::default();
            for ty in intersecting_types.into_iter() {
                let value = self.0.remove(&ty).unwrap();
                intersection.0.insert(ty, value);
            }
            Some(intersection)
        } else {
            None
        };
        self.0.extend(other.0.into_iter());
        prev
    }

    /// Get the type info of the bundle
    pub fn type_info(&self) -> FxHashSet<TypeId> {
        FxHashSet::from_iter(self.0.keys().map(Clone::clone))
    }

    pub fn try_into_tuple<T>(self) -> anyhow::Result<T>
    where
        T: Tuple + 'static,
        T::TupleList: TryFromAnyBundle,
    {
        let tuple_list = <T::TupleList as TryFromAnyBundle>::try_from_any(self)?;
        Ok(tuple_list.into_tuple())
    }

    pub fn from_tuple<T>(tuple: T) -> Self
    where
        T: Tuple,
        AnyBundle: From<T::TupleList>,
    {
        AnyBundle::from(tuple.into_tuple_list())
    }
}

impl From<()> for AnyBundle {
    fn from(_: ()) -> Self {
        AnyBundle::default()
    }
}

impl<Head, Tail> From<(Head, Tail)> for AnyBundle
where
    Head: 'static,
    Tail: TupleList + 'static,
    AnyBundle: From<Tail>,
{
    fn from((head, tail): (Head, Tail)) -> Self {
        let mut bundle = AnyBundle::from(tail);
        let mut store = AnyVec::new::<Head>();
        store.downcast_mut::<Head>().unwrap().push(head);
        bundle.0.insert(store.element_typeid(), store);
        bundle
    }
}

pub trait EmptyBundle {
    fn empty_bundle() -> AnyBundle;
}

impl EmptyBundle for () {
    fn empty_bundle() -> AnyBundle {
        AnyBundle::default()
    }
}

impl<Head, Tail> EmptyBundle for (Head, Tail)
    where
    Head: 'static,
    Tail: EmptyBundle + TupleList
{
    fn empty_bundle() -> AnyBundle {
        let mut bundle = Tail::empty_bundle();
        let ty = TypeId::of::<Head>();
        let anyvec: AnyVec = AnyVec::new::<Head>();
        bundle.0.insert(ty, anyvec);
        bundle
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn bundle_union_returns_prev() {
        let mut left = AnyBundle::from_tuple(("one".to_string(), 1.0f32, true));
        let right = AnyBundle::from_tuple(("two".to_string(), false));
        let leftover: AnyBundle = left.union(right).unwrap();
        let (a, b) = leftover.try_into_tuple::<(String, bool)>().unwrap();
        assert_eq!("one", a.as_str());
        assert_eq!(b, true);

        let (a, b, c) = left.try_into_tuple::<(String, f32, bool)>().unwrap();
        assert_eq!("two", &a);
        assert_eq!(1.0, b);
        assert_eq!(false, c);
    }

    #[test]
    fn ref_bundle() {
        let a: f32 = 0.0;
        let mut b: String = "hello".to_string();
        let c: bool = true;
        let abc = (&a, &mut b, &c);
        let (_a_ref, tail) = abc.into_tuple_list();
        let (b_mut_ref, tail) = tail;
        let (_c_ref, ()) = tail;
        *b_mut_ref = "goodbye".to_string();
    }

    #[test]
    fn type_info_for_tuple() {
        let list_info = type_info::<<(f32, (String, (bool, (u32, ())))) as TupleList>::Tuple>().unwrap();
        let tupl_info = type_info::<(f32, String, bool, u32)>().unwrap();
        assert_eq!(
            list_info,
            tupl_info,
            "diff:\n{:#?}",
            list_info.difference(&tupl_info)
        );

        assert!(type_info::<(f32, bool, f32)>().is_err());
    }

    #[test]
    fn single_tuple_sanity() {
        let _ = type_info::<(f32,)>().unwrap();
    }
}
