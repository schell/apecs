//! System scheduling for outer parallelism.
//!
//! This module contains trait definitions. Implementations can be found in other modeluse.

use std::any::TypeId;

pub(crate) trait IsBorrow: std::fmt::Debug {
    fn type_id(&self) -> TypeId;

    fn name(&self) -> &str;

    fn is_exclusive(&self) -> bool;
}

pub(crate) trait IsSystem: std::fmt::Debug {
    type Borrow: IsBorrow;

    fn name(&self) -> &str;

    fn borrows(&self) -> &[Self::Borrow];

    fn conflicts_with(&self, other: &impl IsSystem) -> bool;
}

/// A batch of systems that can run in parallel (because their borrows
/// don't conflict).
pub(crate) trait IsBatch: std::fmt::Debug + Default {
    type System: IsSystem;

    fn systems(&self) -> &[Self::System];

    fn add_system(&mut self, system: Self::System);
}

pub(crate) trait IsSchedule: std::fmt::Debug {
    type System: IsSystem;
    type Batch: IsBatch<System = Self::System>;

    fn batches_mut(&mut self) -> &mut [Self::Batch];

    fn add_batch(&mut self, batch: Self::Batch);

    fn add_system(&mut self, new_system: Self::System) {
        'batch_loop: for batch in self.batches_mut() {
            for system in batch.systems() {
                if system.conflicts_with(&new_system) {
                    // the new system conflicts with one in
                    // the current batch, break and check
                    // the next batch
                    continue 'batch_loop;
                }
            }

            // the new system doesn't conflict with any existing
            // system, so add it to the batch
            batch.add_system(new_system);
            return;
        }

        // if the system hasn't found a compatible batch, make a new
        // batch and add it
        let mut batch = Self::Batch::default();
        batch.add_system(new_system);
        self.add_batch(batch);
    }
}
