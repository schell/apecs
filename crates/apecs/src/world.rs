//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::{
    collections::HashMap,
    future::Future,
    sync::Arc,
    task::{Context as Ctx, RawWaker, Wake, Waker},
};

use anyhow::Context;

use smol::future::FutureExt;

use crate::{
    mpsc, spsc, AsyncSystem, Borrow, CanFetch, Facade, FetchReadyResource, IsResource, Resource,
    ResourceId, SyncSystem,
};

struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: std::sync::Arc<Self>) {}
}

pub struct World {
    waker: Option<Waker>,
    // world resources
    resources: HashMap<ResourceId, Resource>,
    // resources loaned out to the owner of
    // this world
    loaned_resources: HashMap<ResourceId, Arc<Resource>>,
    // outer resource channel (unbounded)
    resources_from_system: (
        mpsc::Sender<(ResourceId, Resource)>,
        mpsc::Receiver<(ResourceId, Resource)>,
    ),
    sync_systems: Vec<SyncSystem>,
    async_systems: Vec<AsyncSystem>,
    // executor for non-system futures
    executor: smol::Executor<'static>,
    // handles of all non-system futures
    tasks: Vec<smol::Task<()>>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            waker: None,
            resources: Default::default(),
            loaned_resources: HashMap::new(),
            resources_from_system: mpsc::unbounded(),
            sync_systems: vec![],
            async_systems: vec![],
            executor: Default::default(),
            tasks: vec![],
        }
    }
}

fn clear_returned_resources(
    system_name: Option<&str>,
    resources_from_system_rx: &mpsc::Receiver<(ResourceId, Resource)>,
    resources: &mut HashMap<ResourceId, Resource>,
    loaned_resources: &mut HashMap<ResourceId, Arc<Resource>>,
) -> anyhow::Result<()> {
    while let Some((rez_id, resource)) = resources_from_system_rx.try_recv().ok() {
        // put the resources back, there should be nothing stored there currently
        let prev = resources.insert(rez_id, resource);
        if cfg!(feature = "debug_async") && prev.is_some() {
            anyhow::bail!(
                "system '{}' sent back duplicate resources",
                system_name.unwrap_or_else(|| "world")
            );
        }
    }

    // put the borrowed resources back
    if !loaned_resources.is_empty() {
        for (id, rez) in std::mem::take(loaned_resources).into_iter() {
            let rez = Arc::try_unwrap(rez)
                .map_err(|_| anyhow::anyhow!("could not retreive borrowed resource"))?;
            resources.insert(id, rez);
        }
    }

    Ok(())
}

pub(crate) fn try_take_resources<'a>(
    resources: &mut HashMap<ResourceId, Resource>,
    borrows: impl Iterator<Item = &'a Borrow>,
    system_name: Option<&str>,
) -> anyhow::Result<(
    HashMap<ResourceId, FetchReadyResource>,
    HashMap<ResourceId, Arc<Resource>>,
)> {
    // get only the requested resources
    let mut ready_resources: HashMap<ResourceId, FetchReadyResource> = HashMap::default();
    let mut stay_resources: HashMap<ResourceId, Arc<Resource>> = HashMap::default();
    for borrow in borrows {
        let rez: Resource = resources.remove(&borrow.id).with_context(|| {
            format!(
                r#"system '{}' requested missing resource "{}" encountered while building request for {:?}"#,
                system_name.unwrap_or_else(|| "world"),
                borrow.id.name,
                resources
                    .keys()
                    .map(|k| k.name)
                    .collect::<Vec<_>>(),
            )
        })?;

        let ready_rez = if borrow.is_exclusive {
            FetchReadyResource::Owned(rez)
        } else {
            let stay_rez = Arc::new(rez);
            let ready_rez = FetchReadyResource::Ref(stay_rez.clone());
            let _ = stay_resources.insert(borrow.id.clone(), stay_rez);
            ready_rez
        };
        let prev_inserted_rez = ready_resources.insert(borrow.id.clone(), ready_rez);
        assert!(
            prev_inserted_rez.is_none(),
            "cannot request multiple resources of the same type: '{:?}'",
            borrow.id
        );
    }

    Ok((ready_resources, stay_resources))
}

impl World {
    pub fn with_default_resource<T: Default + IsResource>(&mut self) -> anyhow::Result<&mut Self> {
        let resource: T = T::default();
        self.with_resource(resource)
    }

    pub fn with_resource<T: IsResource>(&mut self, resource: T) -> anyhow::Result<&mut Self> {
        if self
            .resources
            .insert(ResourceId::new::<T>(), Box::new(resource))
            .is_some()
        {
            anyhow::bail!("resource {} already exists", std::any::type_name::<T>());
        }

        Ok(self)
    }

    pub fn with_system<T, F>(&mut self, name: impl AsRef<str>, mut sys_fn: F) -> &mut Self
    where
        F: FnMut(T) -> anyhow::Result<()> + 'static,
        T: CanFetch,
    {
        let system = SyncSystem {
            name: name.as_ref().to_string(),
            borrows: T::reads()
                .into_iter()
                .map(|id| Borrow {
                    id,
                    is_exclusive: false,
                })
                .chain(T::writes().into_iter().map(|id| Borrow {
                    id,
                    is_exclusive: true,
                }))
                .collect(),
            function: Box::new(move |tx, mut resources| {
                let data = T::construct(tx, &mut resources)?;
                sys_fn(data)
            }),
        };

        self.sync_systems.push(system);
        self
    }

    pub fn with_async_system<F, Fut>(
        &mut self,
        name: impl AsRef<str>,
        make_system_future: F,
    ) -> &mut Self
    where
        F: FnOnce(Facade) -> Fut,
        Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
    {
        let (resource_request_tx, resource_request_rx) = spsc::unbounded();
        let (resources_to_system_tx, resources_to_system_rx) =
            spsc::bounded::<HashMap<ResourceId, FetchReadyResource>>(1);

        let facade = Facade {
            resource_request_tx,
            resources_to_system_rx,
            resources_from_system_tx: self.resources_from_system.0.clone(),
        };

        let system = AsyncSystem {
            name: name.as_ref().to_string(),
            future: Box::pin((make_system_future)(facade)),
            resource_request_rx,
            resources_to_system_tx,
        };

        self.async_systems.push(system);
        self
    }

    pub fn with_async(
        &mut self,
        future: impl Future<Output = ()> + Send + Sync + 'static,
    ) -> &mut Self {
        self.spawn(future);
        self
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + Send + Sync + 'static) {
        let task = self.executor.spawn(future);
        task.detach();
    }

    pub fn tick(&mut self) -> anyhow::Result<()> {
        let waker = self.waker.take().unwrap_or_else(|| {
            // create a dummy context for asyncs
            let raw_waker = RawWaker::from(Arc::new(DummyWaker));
            unsafe { Waker::from_raw(raw_waker) }
        });
        let mut cx = std::task::Context::from_waker(&waker);

        self.tick_with_context(Some(&mut cx))
    }

    /// Conduct a world tick but use an explicit context.
    /// If no context is given, async systems and futures will not be ticked.
    pub fn tick_with_context<'a>(&mut self, mut cx: Option<&mut Ctx>) -> anyhow::Result<()> {
        tracing::trace!("tick");

        if let Some(cx) = cx.as_mut() {
            self.async_systems.retain_mut(|system| {
                tracing::trace!("running async system '{}'", system.name);
                clear_returned_resources(
                    Some(&system.name),
                    &self.resources_from_system.1,
                    &mut self.resources,
                    &mut self.loaned_resources,
                )
                .unwrap();
                system.tick_async_system(cx, &mut self.resources).unwrap()
            });

            // run the non-system futures and remove the finished tasks
            if !self.tasks.is_empty() {
                // tick the async tasks
                let mut max_ticks = self.tasks.len();
                while max_ticks > 0 && self.executor.try_tick() {
                    max_ticks -= 1;
                }

                for mut task in std::mem::take(&mut self.tasks) {
                    if let std::task::Poll::Pending = task.poll(cx) {
                        self.tasks.push(task);
                    }
                }
            }
        }

        // run through all the systems and poll them all
        for system in self.sync_systems.iter_mut() {
            tracing::trace!("running sync system '{}'", system.name);
            clear_returned_resources(
                Some(&system.name),
                &self.resources_from_system.1,
                &mut self.resources,
                &mut self.loaned_resources,
            )?;
            let (resources, staying_resources) = try_take_resources(
                &mut self.resources,
                system.borrows.iter(),
                Some(&system.name),
            )?;
            (system.function)(self.resources_from_system.0.clone(), resources)?;
            self.loaned_resources.extend(staying_resources);
        }

        Ok(())
    }

    pub fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        clear_returned_resources(
            None,
            &self.resources_from_system.1,
            &mut self.resources,
            &mut self.loaned_resources,
        )?;

        let reads = T::reads().into_iter().map(|id| Borrow {
            id,
            is_exclusive: false,
        });
        let writes = T::writes().into_iter().map(|id| Borrow {
            id,
            is_exclusive: true,
        });
        let borrows = reads.chain(writes).collect::<Vec<_>>();
        let (mut rezs, staying_refs) =
            try_take_resources(&mut self.resources, borrows.iter(), None)?;
        self.loaned_resources.extend(staying_refs);
        T::construct(self.resources_from_system.0.clone(), &mut rezs)
    }

    pub fn new_waker() -> Waker {
        // create a dummy context for asyncs
        let raw_waker = RawWaker::from(Arc::new(DummyWaker));
        unsafe { Waker::from_raw(raw_waker) }
    }

    pub fn get_waker(&mut self) -> Waker {
        self.waker.take().unwrap_or_else(|| Self::new_waker())
    }

    /// Run all system and non-system futures until they have all finished or one
    /// system has erred, whichever comes first.
    pub fn run(&mut self) -> anyhow::Result<&mut Self> {
        let waker = self.get_waker();
        let mut cx = std::task::Context::from_waker(&waker);

        loop {
            self.tick_with_context(Some(&mut cx))?;
            if self.async_systems.is_empty()
                && self.sync_systems.is_empty()
                && self.tasks.is_empty()
            {
                break;
            }
        }

        self.waker = Some(waker);
        Ok(self)
    }
}

#[cfg(test)]
mod test {
    use crate as apecs;
    use apecs::{anyhow, entities::*, join::*, spsc, storage::*, world::*, Read, Write};

    #[test]
    fn can_closure_system() -> anyhow::Result<()> {
        #[derive(CanFetch)]
        struct StatefulSystemData {
            positions: Read<VecStorage<(f32, f32)>>,
        }

        fn mk_stateful_system(
            tx: spsc::Sender<(f32, f32)>,
        ) -> impl FnMut(StatefulSystemData) -> anyhow::Result<()> {
            let mut highest_pos: (f32, f32) = (0.0, f32::NEG_INFINITY);

            move |data: StatefulSystemData| {
                for (_, pos) in (&data.positions,).join() {
                    if pos.1 > highest_pos.1 {
                        highest_pos = *pos;
                    }
                }

                tx.try_send(highest_pos)?;

                Ok(())
            }
        }

        let mut positions: VecStorage<(f32, f32)> = VecStorage::default();
        positions.insert(0, (20.0, 30.0));
        positions.insert(1, (0.0, 0.0));
        positions.insert(2, (100.0, 100.0));

        let (tx, rx) = spsc::bounded(1);

        let mut world = World::default();
        world
            .with_resource(positions)?
            .with_system("stateful", mk_stateful_system(tx));

        world.tick()?;

        let highest = rx.try_recv()?;
        assert_eq!(highest, (100.0, 100.0));

        Ok(())
    }

    #[test]
    fn systems_return_resources() -> anyhow::Result<()> {
        async fn create(mut facade: Facade) -> anyhow::Result<()> {
            let (mut entities, mut names, mut numbers): (
                Write<Entities>,
                Write<VecStorage<String>>,
                Write<VecStorage<u32>>,
            ) = facade.fetch().await?;
            for n in 0..100 {
                let e = entities.create();
                let _ = names.insert(e.id(), format!("entity_{}", n));
                let _ = numbers.insert(e.id(), n);
            }

            Ok(())
        }

        fn maintain_map(mut data: (
            Read<VecStorage<String>>,
            Read<VecStorage<u32>>,
            Write<HashMap<String, u32>>,
        )) -> anyhow::Result<()> {
            for (_, name, number) in (&data.0, &data.1).join() {
                if !data.2.contains_key(name) {
                    let _ = data.2.insert(name.to_string(), *number);
                }
            }

            Ok(())
        }

        let mut world = World::default();
        world
            .with_default_resource::<Entities>()?
            .with_default_resource::<VecStorage<String>>()?
            .with_default_resource::<VecStorage<u32>>()?
            .with_default_resource::<HashMap<String, u32>>()?
            .with_async_system("create", create)
            .with_system("maintain", maintain_map);

        // create system makes fetch request
        world.tick()?;
        // world sends resources to create system which makes ECs,
        // maintain system updates the book
        world.tick()?;

        let book = world.fetch::<Read<HashMap<String, u32>>>()?;
        for n in 0..100 {
            assert_eq!(book.get(&format!("entity_{}", n)), Some(&n));
        }

        Ok(())
    }
}
