# apecs
**A**syncronous **P**arallel **E**ntity **C**omponent **S**ystem

`apecs` is an entity-component system written in Rust that supports traditional syncronous
systems as well as asyncronous systems that can evolve over time. This makes it great for
general applications, quick game prototypes, DIY engines and any simulation that has discrete
steps.

## Why

Most ECS libraries (and game main-loops in general) are polling based. 
This is great for certain tasks, but things get complicated when programming in the time domain.
Async / await is great for programming in the time domain without explicitly spawning new threads or blocking, but it isn't supported by ECS libraries. `apecs` was designed to allow async / await programming within ECS systems. 

## What and How

At its core `apecs` is a library for sharing resources across disparate polling and async loops. 
It uses derivable traits and channels to orchestrate systems' access to resources and uses rayon (where available) for concurrency.

### Asyncronous systems
Async systems are system functions that are `async`. Specifically async systems have this type
signature:
```rust
use apecs::{Facade, anyhow};

async fn my_system(mut facade: Facade) -> anyhow::Result<()> {
    //...
    Ok(())
}
```
The `Facade` type is like a window into the world. It can visit bundles of resources in the
world asyncronously. This allows your async system to affect different parts of the
world at different times.

Syncronous systems are great for tight loops that are always iterating over the same
data. In other words sync systems are highly optimized algorithms that run in the hot path.
But they don't quite fit in situations where the system's focus changes over time, or when
the system needs to wait for some condition before doing something different. Async systems are a
good fit for these situations. Async systems are therefore a good fit for high-level orchestration.

For example you might use an async system to setup your title screen, wait for user input and then
start the main game simulation by injecting your game entities.

Another example might be an app that has a dependency graph of work to complete. An Async system
can hold the dependencies as a series of async operations that it is awaiting, while syncronous
systems do the hot-path work that completes those async operations as fast as possible.

## Goals
* productivity
* flexibility
* observability
* very well rounded performance, competitive with inspirational ECS libraries
  - like `specs`, `bevy_ecs`, `hecs`, `legion`, `shipyard`, `planck_ecs`

## Features
- syncronous systems with early exit and failure
  ```rust
  use apecs::*;

  #[derive(Clone, Copy, Debug, Default, PartialEq)]
  struct Number(u32);

  let mut world = World::default();
  world
      .with_system("demo", |mut u32_number: Write<Number>| -> anyhow::Result<ShouldContinue> {
          u32_number.0 += 1;
          if u32_number.0 == 3 {
              end()
          } else {
              ok()
          }
      })
      .unwrap();
  world.run();
  assert_eq!(Number(3), *world.resource::<Number>().unwrap());
  ```
- async systems, ie systems that end and/or change over time (for scenes, stories, etc)
  - fetch and visit resources from the world asyncronously. If they have not been added and can be
    created by default, they will be. `Write` and `Read` will create default resources
    during fetching if possible.
  - resources are acquired without lifetimes
  - when fetched resources are dropped they are sent back into the world
  ```rust
  use apecs::*;

  #[derive(Clone, Copy, Debug, Default, PartialEq)]
  struct Number(u32);

  async fn demo(mut facade: Facade) -> anyhow::Result<()> {
      loop {
          let i = facade.visit(|mut u32_number: Write<Number>| {
              u32_number.0 += 1;
              Ok(u32_number.0)
          }).await?;
          if i > 5 {
              break;
          }
      }
      Ok(())
  }

  let mut world = World::default();
  world
      .with_async("demo", demo)
      .unwrap();
  world.run();
  assert_eq!(Number(6), *world.resource::<Number>().unwrap());
  ```
- support for async futures
  ```rust
  use apecs::*;
  let mut world = World::default();
  world
      .spawn(async {
          log::trace!("hello");
      });
  world.run();
  ```
- fetch data (system data) derive macros
  ```rust
  use apecs::*;

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
  - systems may depend on other systems running before or after
  - barriers
  ```rust
  use apecs::*;

  fn one(mut u32_number: Write<u32>) -> anyhow::Result<ShouldContinue> {
      *u32_number += 1;
      end()
  }

  fn two(mut u32_number: Write<u32>) -> anyhow::Result<ShouldContinue> {
      *u32_number += 1;
      end()
  }

  fn exit_on_three(mut f32_number: Write<f32>) -> anyhow::Result<ShouldContinue> {
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
      // one should run before two
      .with_system_with_dependencies("one", one, &[], &["two"]).unwrap()
      // two should run after one - this is redundant but good for illustration
      .with_system_with_dependencies("two", two, &["one"], &[]).unwrap()
      // exit_on_three has no dependencies
      .with_system("run_thrice_and_leave", exit_on_three).unwrap()
      // all systems after a barrier run after the systems before a barrier
      .with_system_barrier()
      .with_system("lastly", lastly).unwrap();

  assert_eq!(
      vec![
          vec!["one"],
          vec!["run_thrice_and_leave", "two"],
          vec!["lastly"],
      ],
      world.get_sync_schedule_names()
  );

  world.tick();
  assert_eq!(
      vec![
          vec!["run_thrice_and_leave"],
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
  use apecs::*;

  // Make a type for tracking changes
  #[derive(Default)]
  struct MyTracker(u64);

  // Entities and Components (which stores components) are default
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
      }, &["create"], &[]).unwrap()
      .with_system_with_dependencies(
          "sync",
          |(q_others, mut tracker): (Query<(&f32, &mut String, &mut u32)>, Write<MyTracker>)| {
              for (f32, string, u32) in q_others.query().iter_mut() {
                  if f32.was_modified_since(tracker.0) {
                      **u32 = **f32 as u32;
                      **string = format!("{}:{}", f32.id(), **u32);
                  }
              }
              tracker.0 = apecs::current_iteration();
              ok()
          },
          &["progress"],
          &[]
      ).unwrap();

  assert_eq!(
      vec![
          vec!["create"],
          vec!["progress"],
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
  use apecs::*;

  #[derive(Default)]
  struct F32(f32);

  let mut world = World::default();
  world
      .with_system("one", |mut f32_number: Write<F32>| {
          f32_number.0 += 1.0;
          ok()
      }).unwrap()
      .with_system("two", |f32_number: Read<F32>| {
          println!("system two reads {}", f32_number.0);
          ok()
      }).unwrap()
      .with_system("three", |f32_number: Read<F32>| {
          println!("system three reads {}", f32_number.0);
          ok()
      }).unwrap()
      .with_parallelism(Parallelism::Automatic);
  world.tick();
  ```
- plugins (groups of systems, resources and sub-plugins)
  ```rust
  use apecs::*;

  #[derive(Default)]
  struct MyTracker(u64);

  #[derive(CanFetch)]
  struct SyncData {
      q_dirty_f32s: Query<(&'static f32, &'static mut String)>,
      tracker: Write<MyTracker>,
  }

  fn create(
      (mut entities, mut components): (Write<Entities>, Write<Components>),
  ) -> anyhow::Result<ShouldContinue> {
      // create a bunch of entities in bulk, which is quite fast
      let ids = entities.create_many(1000);
      components.extend::<(f32, String)>((
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
      data.tracker.0 = apecs::current_iteration();
      ok()
  }

  // now we can package it all up into a plugin
  let my_plugin = Plugin::default()
      .with_system("create", create, &[], &[])
      .with_system("progress", progress, &["create"], &[])
      .with_system("sync", sync, &[], &[]);

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

I like firefox, but you can use different browsers for the wasm tests. The tests
make sure apecs works on wasm.

## Benchmarks
The `apecs` benchmarks measure itself against my favorite ECS libs:
`specs`, `bevy`, `hecs`, `legion`, `shipyard` and `planck_ecs`.

```bash
cargo bench -p benchmarks
```

# Minimum supported Rust version 1.65
`apecs` uses generic associated types for its component iteration traits.
