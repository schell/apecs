# apecs
**A**syncronous **P**arallel **E**ntity **C**omponent **S**ystem

## Why
`apecs` is an entity-component system written in Rust that supports traditional syncronous
systems as well as asyncronous systems that can evolve over time. This makes it great for
general applications, quick game prototypes, DIY engines and any simulation that has discrete
steps.

## Goals
* productivity
* flexibility
* observability
* very well rounded performance, competitive with inspirational ECS libraries
  - like `specs`, `bevy_ecs`, `hecs`, `legion`, `shipyard`, `planck_ecs`

## Features
- syncronous systems with early exit and failure
  ```rust
  use apecs::{anyhow, Write, world::*, system::*};
  let mut world = World::default();
  world
      .with_system("demo", |mut u32_number: Write<u32>| -> anyhow::Result<ShouldContinue> {
          *u32_number += 1;
          if *u32_number == 3 {
              end()
          } else {
              ok()
          }
      })
      .unwrap();
  world.run();
  assert_eq!(3, *world.resource::<u32>().unwrap());
  ```
- async systems, ie systems that end and/or change over time (for scenes, stories, etc)
  - fetch resources from the world asyncronously. If they have not been added and can be
    created by default, they will be. `Write` and `Read` will create default resources
    during fetching, and `WriteExpect` and `ReadExpect` will not.
  - resources are acquired without lifetimes
  - when resources are dropped they are sent back into the world
  ```rust
  use apecs::{anyhow, Write, world::*};
  async fn demo(mut facade: Facade) -> anyhow::Result<()> {
      loop {
          let mut u32_number: Write<u32> = facade.fetch().await?;
          *u32_number += 1;
          if *u32_number > 5 {
              break;
          }
      }
      Ok(())
  }
  let mut world = World::default();
  world
      .with_async_system("demo", demo);
  world.run();
  assert_eq!(6, *world.resource::<u32>().unwrap());
  ```
- support for async futures
  ```rust
  use apecs::world::World;
  let mut world = World::default();
  world
      .with_async(async {
          log::trace!("hello");
      });
  world.run();
  ```
- fetch data (system data) derive macros
  ```rust
  use apecs::{CanFetch, Read, Write, world::*};

  #[derive(CanFetch)]
  struct MyData {
      entities: Read<Entities>,
      u32_number: Write<u32>,
  }

  let mut world = World::default();
  let mut my_data: MyData = world.fetch().unwrap();
  *my_data.u32_number = 1;
  ```
- system scheduling
  - compatible systems are placed in parallel batches (a batch is a group of systems
    that can run in parallel, ie they don't have conflicting borrows)
  - systems may depend on other systems running first
  - barriers
  ```rust
  use apecs::{Read, Write, system::*, world::*};

  fn one(mut u32_number: Write<u32>) -> anyhow::Result<ShouldContinue> {
      *u32_number += 1;
      end()
  }

  fn two(mut u32_number: Write<u32>) -> anyhow::Result<ShouldContinue> {
      *u32_number += 1;
      end()
  }

  fn thrice(mut f32_number: Write<f32>) -> anyhow::Result<ShouldContinue> {
      *f32_number += 1.0;
      if *f32_number == 3.0 {
          end()
      } else {
          ok()
      }
  }

  fn lastly((u32_number, f32_number): (Read<u32>, Read<f32>)) -> anyhow::Result<ShouldContinue> {
      if *u32_number == 2 && *f32_number == 3.0 {
          end()
      } else {
          ok()
      }
  }

  let mut world = World::default();
  world
      .with_system("one", one).unwrap()
      .with_system_with_dependencies("two", two, &["one"]).unwrap()
      .with_system("thrice", thrice).unwrap()
      .with_system_barrier()
      .with_system("lastly", lastly).unwrap();

  assert_eq!(
      vec![
          vec!["one", "thrice"],
          vec!["two"],
          vec!["lastly"],
      ],
      world.get_sync_schedule_names()
  );

  world.tick();
  assert_eq!(
      vec![
          vec!["thrice"],
          vec!["lastly"],
      ],
      world.get_sync_schedule_names()
  );

  world.tick();
  world.tick();
  assert!(world.get_sync_schedule_names().is_empty());
  ```
- component storage
  - optimized for space and iteration time as archetypes
  - queries with "maybe" and "without" semantics
  - queries can find a single entity without iteration or filtering
  - add and modified time tracking
  - parallel queries (inner parallelism)
  ```rust
  use apecs::{world::*, storage::*, system::*, Read, Write};

  // Make a type for tracking changes
  #[derive(Default)]
  struct MyTracker(u64);

  // Entities and ArchetypeSet (which stores components) are default
  // resources
  let mut world = World::default();
  world
      .with_system("create", |mut entities: Write<Entities>| {
          for mut entity in (0..100).map(|_| entities.create()) {
              entity.insert_bundle((0.0f32, 0u32, format!("{}:0", entity.id())));
          }
          end()
      }).unwrap()
      .with_system_with_dependencies("progress", |q_f32s: Query<&mut f32>| {
          for f32 in q_f32s.query().iter_mut() {
              **f32 += 1.0;
          }
          ok()
      }, &["create"]).unwrap()
      .with_system_with_dependencies(
          "sync",
          |(q_others, mut tracker): (Query<(&f32, &mut String, &mut u32)>, Write<MyTracker>)| {
              for (f32, string, u32) in q_others.query().iter_mut() {
                  if f32.was_modified_since(tracker.0) {
                      **u32 = **f32 as u32;
                      **string = format!("{}:{}", f32.id(), **u32);
                  }
              }
              tracker.0 = apecs::system::current_iteration();
              ok()
          },
          &["progress"]
      ).unwrap();

  assert_eq!(
      vec![
          vec!["create", "progress"], // <- same batch because they don't share exclusively
          vec!["sync"],
      ],
      world.get_sync_schedule_names()
  );

  world.tick(); // entities are created, components applied lazily
  world.tick(); // f32s are modified, u32s and strings are synced
  world.tick(); // f32s are modified, u32s and strings are synced

  let q_bundle: Query<(&f32, &u32, &String)> = world.fetch().unwrap();
  assert_eq!(
      (2.0f32, 2u32, "13:2".to_string()),
      q_bundle.query().find_one(13).map(|(f, u, s)| (**f, **u, s.to_string())).unwrap()
  );
  ```
- outer parallelism (running systems in parallel)
  - parallel system scheduling
  - parallel execution of async futures
  - parallelism is configurable (can be automatic or a requested number of threads, including 1)
  ```rust
  use apecs::{system::*, world::*, Write, Read};
  let mut world = World::default();
  world
      .with_system("one", |mut f32_number: Write<f32>| {
          *f32_number += 1.0;
          ok()
      }).unwrap()
      .with_system("two", |f32_number: Read<f32>| {
          println!("system two reads {}", *f32_number);
          ok()
      }).unwrap()
      .with_system("three", |f32_number: Read<f32>| {
          println!("system three reads {}", *f32_number);
          ok()
      }).unwrap()
      .with_parallelism(Parallelism::Automatic);
  world.tick();
  ```
- plugins (groups of systems, resources and sub-plugins)
  ```rust
  use apecs::{plugins::*, storage::*, system::*, world::*, CanFetch, Write};

  #[derive(Default)]
  struct MyTracker(u64);

  #[derive(CanFetch)]
  struct SyncData {
      q_dirty_f32s: Query<(&'static f32, &'static mut String)>,
      tracker: Write<MyTracker>,
  }

  fn create(
      (mut entities, mut archeset): (Write<Entities>, Write<ArchetypeSet>),
  ) -> anyhow::Result<ShouldContinue> {
      // create a bunch of entities in bulk, which is quite fast
      let ids = entities.create_many(1000);
      archeset.extend::<(f32, String)>((
          Box::new(ids.clone().into_iter().map(|id| Entry::new(id, id as f32))),
          Box::new(
              ids.into_iter()
                  .map(|id| Entry::new(id, format!("{}:{}", id, id))),
          ),
      ));
      end()
  }

  fn progress(q: Query<&mut f32>) -> anyhow::Result<ShouldContinue> {
      for f32 in q.query().iter_mut() {
          **f32 += 10.0;
      }
      ok()
  }

  fn sync(mut data: SyncData) -> anyhow::Result<ShouldContinue> {
      for (f32, string) in data.q_dirty_f32s.query().iter_mut() {
          if f32.was_modified_since(data.tracker.0) {
              **string = format!("{}:{}", f32.id(), **f32);
          }
      }
      data.tracker.0 = apecs::system::current_iteration();
      ok()
  }

  // now we can package it all up into a plugin
  let my_plugin = Plugin::default()
      .with_system("create", create, &[])
      .with_system("progress", progress, &["create"])
      .with_system("sync", sync, &[]);

  let mut world = World::default();
  world.with_plugin(my_plugin).unwrap();
  world.tick();
  ```
- fully compatible with WASM and runs in the browser

## Roadmap
- your ideas go here

## Tests
```bash
cargo test
wasm-pack test --firefox crates/apecs
```

I like firefox, but you can use different browsers for the wasm tests. For the most part they're there
just to make sure apecs works on wasm.

## Benchmarks
The `apecs` benchmarks measure itself against my favorite ECS libs:
`specs`, `bevy`, `hecs`, `legion`, `shipyard` and `planck_ecs`.

```bash
cargo bench -p benchmarks
```

# Caveats
- `apecs` uses generic associated types. This means it can only be compiled with nightly.
