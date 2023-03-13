//! Manages system resources.
//!
//! This module should not need to be used by most users of this library.
use anyhow::Context;
use broomdog::TypeMap;

use crate::Gen;

use super::{
    internal::{Borrow, FetchReadyResource, TypeValue},
    IsResource, TypeKey,
};

/// Performs loans on behalf of the world.
#[derive(Default)]
pub struct ResourceManager {
    pub type_map: TypeMap,
}

impl ResourceManager {
    pub fn add<T: IsResource>(&mut self, rez: T) -> Option<TypeValue> {
        self.insert(TypeKey::new::<T>(), TypeValue::new(rez))
    }

    pub fn insert(&mut self, id: TypeKey, boxed_rez: TypeValue) -> Option<TypeValue> {
        self.type_map.insert(id, boxed_rez)
    }

    pub fn has_resource(&self, key: &TypeKey) -> bool {
        self.type_map.contains_key(key)
    }

    pub fn get<T: IsResource>(&self) -> anyhow::Result<&T> {
        let may_t = self.type_map.get_value::<T>()?;
        let t = may_t.with_context(|| format!("missing {}", std::any::type_name::<T>()))?;
        Ok(t)
    }

    pub fn get_mut<T: IsResource>(&mut self) -> anyhow::Result<&mut T> {
        let may_t = self.type_map.get_value_mut::<T>()?;
        let t =
            may_t.with_context(|| format!("resource {} is missing", std::any::type_name::<T>()))?;
        Ok(t)
    }

    fn missing_msg(label: &str, borrow: &Borrow, extra: &str) -> String {
        format!(
            r#"'{}' requested missing resource "{}" encountered while building request: {}
"#,
            label,
            borrow.rez_id().name(),
            extra
        )
    }

    pub fn get_loaned(
        &mut self,
        label: &str,
        borrow: &Borrow,
    ) -> anyhow::Result<FetchReadyResource> {
        let rez_id = borrow.rez_id();
        Ok(if borrow.is_exclusive() {
            let may_loan = self.type_map.loan_mut(rez_id)?;
            anyhow::ensure!(
                may_loan.is_some(),
                Self::missing_msg(label, &borrow, "resource has not been inserted")
            );
            log::trace!("loaning exclusive {}", rez_id.name());
            FetchReadyResource::Owned(may_loan.unwrap())
        } else {
            let may_loan = self.type_map.loan(rez_id)?;
            anyhow::ensure!(
                may_loan.is_some(),
                Self::missing_msg(label, &borrow, "resource has not been inserted")
            );
            log::trace!("loaning {}", rez_id.name());
            FetchReadyResource::Ref(may_loan.unwrap())
        })
    }

    /// Unify all resources by collecting them from loaned refs and
    /// the return channel.
    ///
    /// If the resources cannot be unified this function will err.
    /// Use `try_unify_resources` if you would like not to err.
    pub fn unify_resources(&mut self, label: &str) -> anyhow::Result<()> {
        log::trace!("unify resources {}", label);
        self.type_map.unify()?;
        Ok(())
    }

    /// Attempt to unify all resources by collecting them from return channels
    /// and unwrapping shared references.
    ///
    /// Returns `Ok(true)` if resources are still on loan.
    ///
    /// Errs if a system returns a resource that was not loaned. This doesn't
    /// actually happen.
    pub fn try_unify_resources(&mut self, label: &str) -> anyhow::Result<bool> {
        log::trace!("try unify resources {}", label);
        let err = self.type_map.unify().is_err();
        Ok(err)
    }

    /// Mutably borrow as a [`LoanManager`].
    pub fn as_mut_loan_manager(&mut self) -> LoanManager<'_> {
        LoanManager(self)
    }
}

/// Manages resources requested by systems. For internal use, mostly.
pub struct LoanManager<'a>(pub(crate) &'a mut ResourceManager);

impl<'a> LoanManager<'a> {
    /// Attempt to get a loan on the resources specified by the given `Borrow`.
    pub fn get_loaned(
        &mut self,
        label: &str,
        borrow: &Borrow,
    ) -> anyhow::Result<FetchReadyResource> {
        self.0.get_loaned(label, borrow)
    }

    pub fn get_loaned_or_gen<T: IsResource, G: Gen<T>>(
        &mut self,
        label: &str,
        borrow: &Borrow,
    ) -> anyhow::Result<FetchReadyResource> {
        let rez_id = TypeKey::new::<T>();
        if self.0.has_resource(&rez_id) {
            self.0.get_loaned(label, borrow)
        } else {
            log::trace!(
                "{} was missing in resources, so we'll try to create it from default",
                std::any::type_name::<T>()
            );
            let t: T = G::generate()
                .with_context(|| format!("could not make default value for {}", rez_id.name()))?;
            let prev = self
                .0
                .insert(TypeKey::new::<T>(), TypeValue::from(Box::new(t)));
            debug_assert!(prev.is_none());
            self.0.get_loaned(label, borrow)
        }
    }

    /// Attempt to get a mutable reference to a resource.
    pub fn get_mut<T: IsResource>(&mut self) -> anyhow::Result<&mut T> {
        self.0.get_mut::<T>()
    }
}

#[cfg(test)]
mod test {
    use crate::{CanFetch, Write};

    use super::*;

    #[derive(Default)]
    struct MyVec(Vec<&'static str>);
    impl MyVec {
        fn push(&mut self, s: &'static str) {
            self.0.push(s);
        }
    }

    #[test]
    fn can_get_mut() {
        let mut mngr = ResourceManager::default();
        assert!(mngr.add(MyVec(vec!["one", "two"])).is_none());
        {
            let vs: &mut MyVec = mngr.get_mut::<MyVec>().unwrap();
            vs.push("three");
        }
    }

    #[test]
    fn can_fetch_roundtrip() {
        let mut mngr = ResourceManager::default();
        mngr.add(MyVec(vec!["one"]));
        {
            let mut vs = Write::<MyVec>::construct(&mut LoanManager(&mut mngr)).unwrap();
            vs.push("two");
        }
        mngr.unify_resources("test").unwrap();
        {
            let mut vs = Write::<MyVec>::construct(&mut LoanManager(&mut mngr)).unwrap();
            vs.push("three");
        }
        mngr.unify_resources("test").unwrap();
    }
}
