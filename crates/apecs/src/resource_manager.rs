//! Manages system resources.
//!
//! This module should not need to be used by most users of this library.
use std::sync::Arc;

use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::Gen;

use super::{
    chan::mpsc,
    internal::{FetchReadyResource, Resource, Borrow},
    IsResource, ResourceId,
};

/// Systems use this to return resources borrowed exclusively.
pub struct ExclusiveResourceReturnChan(
    mpsc::Sender<(ResourceId, Resource)>,
    mpsc::Receiver<(ResourceId, Resource)>,
);

/// Performs loans on behalf of the world.
pub struct ResourceManager {
    // Resources held for the world's systems
    pub world_resources: FxHashMap<ResourceId, Resource>,
    // Resources currently on loan from this struct to the world's systems
    //
    // A system borrowing a resource in this field only has a reference to the
    // resources.
    pub loaned_refs: FxHashMap<ResourceId, Arc<Resource>>,
    // Resources currently on loan from this struct to the world's systems.
    //
    // A system borrowing a resource in this field has an exclusive, mutable
    // borrow to the resource.
    pub loaned_muts: FxHashSet<ResourceId>,
    // A channel that systems use to return loaned resources
    pub exclusive_return_chan: ExclusiveResourceReturnChan,
}

impl Default for ResourceManager {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded();
        Self {
            world_resources: Default::default(),
            loaned_refs: Default::default(),
            loaned_muts: Default::default(),
            exclusive_return_chan: ExclusiveResourceReturnChan(tx, rx),
        }
    }
}

impl ResourceManager {
    /// Get a sender that can be used to return resources.
    pub fn exclusive_resource_return_sender(&self) -> mpsc::Sender<(ResourceId, Resource)> {
        self.exclusive_return_chan.0.clone()
    }

    pub fn add<T: IsResource>(&mut self, rez: T) -> Option<Resource> {
        self.insert(ResourceId::new::<T>(), Resource::from(Box::new(rez)))
    }

    pub fn insert(&mut self, id: ResourceId, boxed_rez: Resource) -> Option<Resource> {
        self.world_resources.insert(id, boxed_rez)
    }

    pub fn has_resource(&self, key: &ResourceId) -> bool {
        self.world_resources.contains_key(key)
            || self.loaned_refs.contains_key(key)
            || self.loaned_muts.contains(key)
    }

    pub fn get<T: IsResource>(&self, key: &ResourceId) -> anyhow::Result<&T> {
        let rez: &Resource = self
            .world_resources
            .get(key)
            .with_context(|| format!("resource {} is missing", key.name))?;
        rez.downcast_ref()
    }

    pub fn get_mut<T: IsResource>(&mut self) -> anyhow::Result<&mut T> {
        let key = ResourceId::new::<T>();
        let rez: &mut Resource = self
            .world_resources
            .get_mut(&key)
            .with_context(|| format!("resource {} is missing", key.name))?;
        rez.downcast_mut()
    }

    fn missing_msg(label: &str, borrow: &Borrow, extra: &str) -> String {
        format!(
            r#"'{}' requested missing resource "{}" encountered while building request: {}
"#,
            label,
            borrow.rez_id().name,
            extra
        )
    }

    pub fn get_loaned(
        &mut self,
        label: &str,
        borrow: &Borrow,
    ) -> anyhow::Result<FetchReadyResource> {
        let rez_id = borrow.rez_id();
        let ready_rez: FetchReadyResource = match self.world_resources.remove(&rez_id) {
            Some(rez) => {
                if borrow.is_exclusive() {
                    assert!(self.loaned_muts.insert(rez_id), "already mutably borrowed",);
                    FetchReadyResource::Owned(rez)
                } else {
                    let arc_rez = Arc::new(rez);
                    if !self.loaned_refs.contains_key(&rez_id) {
                        let _ = self.loaned_refs.insert(rez_id, arc_rez.clone());
                    }
                    FetchReadyResource::Ref(arc_rez)
                }
            }
            None => {
                // it's not in the main map, so maybe it was previously borrowed
                if borrow.is_exclusive() {
                    anyhow::bail!(Self::missing_msg(
                        label,
                        &borrow,
                        "the borrow is exclusive and the resource is missing"
                    ))
                } else {
                    let rez: &Arc<Resource> =
                        self.loaned_refs.get(&rez_id).context(Self::missing_msg(
                            label,
                            &borrow,
                            "the borrow is not exclusive but the resource is missing from \
                             previously borrowed resources",
                        ))?;
                    FetchReadyResource::Ref(rez.clone())
                }
            }
        };

        Ok(ready_rez)
    }

    /// Returns whether any resources are out on loan
    pub fn are_any_resources_on_loan(&self) -> bool {
        !(self.loaned_muts.is_empty() && self.loaned_refs.is_empty())
    }

    /// Unify all resources by collecting them from loaned refs and
    /// the return channel.
    ///
    /// If the resources cannot be unified this function will err.
    /// Use `try_unify_resources` if you would like not to err.
    pub fn unify_resources(&mut self, label: &str) -> anyhow::Result<()> {
        log::trace!("unify resources {}", label);
        let resources_are_still_loaned = self.try_unify_resources(label)?;
        if resources_are_still_loaned {
            anyhow::bail!(
                "{} cannot unify resources, some are still on loan:\n{}",
                label,
                self.resources_on_loan_msg()
            )
        }

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
        while let Ok((rez_id, resource)) = self.exclusive_return_chan.1.try_recv() {
            // put the exclusively borrowed resources back, there should be nothing stored
            // there currently
            let prev = self.world_resources.insert(rez_id.clone(), resource);
            debug_assert!(prev.is_none());
            anyhow::ensure!(
                self.loaned_muts.remove(&rez_id),
                "{} was not removed from loaned_muts",
                rez_id.name
            );
        }

        // put the loaned ref resources back
        if self.are_any_resources_on_loan() {
            for (id, rez) in std::mem::take(&mut self.loaned_refs).into_iter() {
                match Arc::try_unwrap(rez) {
                    Ok(rez) => anyhow::ensure!(
                        self.world_resources.insert(id, rez).is_none(),
                        "duplicate resources"
                    ),
                    Err(arc_rez) => {
                        log::error!(
                            "could not retreive borrowed resource {:?}, it is still borrowed by \
                             '{}' - do not to hold loaned resources over an await point",
                            id.name, label,
                        );
                        let _ = self.loaned_refs.insert(id, arc_rez);
                    }
                }
            }
        }

        Ok(self.are_any_resources_on_loan())
    }

    /// Returns a message about resources out on loan.
    pub fn resources_on_loan_msg(&self) -> String {
        self.loaned_refs
            .keys()
            .map(|id| format!("  & {}", id.name))
            .chain(self.loaned_muts.iter().map(|id| format!("    {}", id.name)))
            .collect::<Vec<_>>()
            .join("\n")
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
        let rez_id = ResourceId::new::<T>();
        if self.0.has_resource(&rez_id) {
            self.0.get_loaned(label, borrow)
        } else {
            log::trace!(
                "{} was missing in resources, so we'll try to create it from default",
                std::any::type_name::<T>()
            );
            let t: T = G::generate()
                .with_context(|| format!("could not make default value for {}", rez_id.name))?;
            let prev = self
                .0
                .insert(ResourceId::new::<T>(), Resource::from(Box::new(t)));
            debug_assert!(prev.is_none());
            self.0.get_loaned(label, borrow)
        }
    }

    /// Attempt to get a mutable reference to a resource.
    pub fn get_mut<T: IsResource>(&mut self) -> anyhow::Result<&mut T> {
        self.0.get_mut::<T>()
    }

    /// Get a clone of the resource return channel sender.
    pub fn resource_return_tx(&self) -> mpsc::Sender<(ResourceId, Resource)> {
        self.0.exclusive_resource_return_sender()
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
