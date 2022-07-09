//! Wrangles the complexity of managing system resources.
use std::sync::Arc;

use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{mpsc, schedule::Borrow, FetchReadyResource, IsResource, Resource, ResourceId};

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

impl ResourceManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded();
        Self {
            world_resources: Default::default(),
            loaned_refs: Default::default(),
            loaned_muts: Default::default(),
            exclusive_return_chan: ExclusiveResourceReturnChan(tx, rx),
        }
    }

    /// Get a sender that can be used to return resources.
    pub fn exclusive_resource_return_sender(&self) -> mpsc::Sender<(ResourceId, Resource)> {
        self.exclusive_return_chan.0.clone()
    }

    pub fn add<T: IsResource>(&mut self, rez: T) -> Option<Resource> {
        self.insert(ResourceId::new::<T>(), Box::new(rez))
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
        let box_t: &Resource = self
            .world_resources
            .get(key)
            .with_context(|| format!("resource {} is missing", key.name))?;
        box_t
            .downcast_ref()
            .with_context(|| "could not downcast resource")
    }

    pub fn get_mut<T: IsResource>(&mut self, key: &ResourceId) -> anyhow::Result<&mut T> {
        let box_t: &mut Resource = self
            .world_resources
            .get_mut(key)
            .with_context(|| format!("resource {} is missing", key.name))?;
        box_t
            .downcast_mut()
            .with_context(|| "could not downcast resource")
    }

    /// Attempt to loan the requested resources by adding them to the given
    /// mutable hash map.
    pub fn try_loan_resources<'a>(
        &mut self,
        label: &str,
        additive_map: &mut FxHashMap<ResourceId, FetchReadyResource>,
        borrows: impl Iterator<Item = &'a Borrow>,
    ) -> anyhow::Result<()> {
        for borrow in borrows {
            let missing_msg = |extra: &str| {
                format!(
                    r#"'{}' requested missing resource "{}" encountered while building request\n{}
"#,
                    label,
                    borrow.rez_id().name,
                    extra
                )
            };

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
                        anyhow::bail!(missing_msg(
                            "the borrow is exclusive and the resource is missing"
                        ))
                    } else {
                        let rez: &Arc<Resource> =
                            self.loaned_refs.get(&rez_id).context(missing_msg(
                                "the borrow is not exclusive but the resource is missing from \
                                 previously borrowed resources",
                            ))?;
                        FetchReadyResource::Ref(rez.clone())
                    }
                }
            };

            let prev_inserted_rez = additive_map.insert(borrow.rez_id(), ready_rez);
            anyhow::ensure!(
                prev_inserted_rez.is_none(),
                "cannot request multiple resources of the same type: '{:?}'",
                borrow.rez_id()
            );
        }

        Ok(())
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
    pub fn try_unify_resources(&mut self, label: &str) -> anyhow::Result<bool> {
        log::trace!("try unify resources {}", label);
        while let Ok((rez_id, resource)) = self.exclusive_return_chan.1.try_recv() {
            // put the exclusively borrowed resources back, there should be nothing stored
            // there currently
            let prev = self.world_resources.insert(rez_id.clone(), resource);
            if cfg!(feature = "debug-async") && prev.is_some() {
                anyhow::bail!("'{}' sent back duplicate resources", label);
            }
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
                        log::warn!(
                            "could not retreive borrowed resource {:?}, it is still borrowed by \
                             {} - for better performance, try not to hold loaned resources over \
                             an await point",
                            id.name,
                            label,
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
}

#[cfg(test)]
mod test {
    use crate::{CanFetch, Write};

    use super::*;

    #[test]
    fn can_get_mut() {
        let mut mngr = ResourceManager::new();
        assert!(mngr.add(vec!["one", "two"]).is_none());
        {
            let vs: &mut Vec<&str> = mngr.get_mut(&ResourceId::new::<Vec<&str>>()).unwrap();
            vs.push("three");
        }
    }

    #[test]
    fn can_fetch_roundtrip() {
        let mut mngr = ResourceManager::new();
        mngr.add(vec!["one"]);

        let mut borrows = Write::<Vec<&str>>::reads()
            .into_iter()
            .map(|id| Borrow {
                id,
                is_exclusive: false,
            })
            .collect::<Vec<_>>();
        borrows.extend(Write::<Vec<&str>>::writes().into_iter().map(|id| Borrow {
            id,
            is_exclusive: true,
        }));

        let mut ready_rezs = FxHashMap::default();
        mngr.try_loan_resources("test", &mut ready_rezs, borrows.iter())
            .unwrap();

        {
            let mut vs = Write::<Vec<&str>>::construct(
                mngr.exclusive_resource_return_sender(),
                &mut ready_rezs,
            )
            .unwrap();
            vs.push("two");
        }

        mngr.unify_resources("test").unwrap();
        mngr.try_loan_resources("test", &mut ready_rezs, borrows.iter())
            .unwrap();

        {
            let mut vs = Write::<Vec<&str>>::construct(
                mngr.exclusive_resource_return_sender(),
                &mut ready_rezs,
            )
            .unwrap();
            vs.push("three");
        }

        mngr.unify_resources("test").unwrap();
    }
}
