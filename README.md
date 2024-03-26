# apecs
**A**sync-friendly and **P**leasant **E**ntity **C**omponent **S**ystem

`apecs` is an entity-component system written in Rust that can share world resources with 
futures run in any async runtime. This makes it great for general applications, 
quick game prototypes, DIY engines and any simulation that has discrete steps in time.

## Why

Most ECS libraries (and game main-loops in general) are polling based. 
This is great for certain tasks, but things get complicated when programming in the time domain.
Async / await is great for programming in the time domain without explicitly spawning new threads 
or blocking, but it isn't supported by ECS libraries. 

`apecs` was designed to to be an ECS that plays nice with async / await. 

## What and How

At its core `apecs` is a library for sharing resources across disparate polling and async loops. 
It uses derivable traits and channels to orchestrate systems' access to resources and uses rayon 
(where available) for concurrency.

## Goals
* productivity
* flexibility
* observability
* very well rounded performance, competitive with inspirational ECS libraries
  - like `specs`, `bevy_ecs`, `hecs`, `legion`, `shipyard`, `planck_ecs`
  - backed by criterion benchmarks

## Features

Here is a quick table of features compared to other ECSs.

| Feature           | apecs    | bevy_ecs | hecs     | legion   | planck_ecs | shipyard | specs     |
|-------------------|----------|----------|----------|----------|------------|----------|-----------|
| storage           |archetypal|  hybrid  |archetypal|archetypal| separated  |  sparse  | separated |
| system scheduling | ✔️        | ✔️       |          | ✔️        | ✔️          | ✔️        | ✔️         |
| early exit systems| ✔️        |          |          |          |            |          |           |
| parallel systems  | ✔️        | ✔️       | ✔️        | ✔️        |            | ✔️        | ✔️         |
| change tracking   | ✔️        | ✔️       |          | kinda    |            | ✔️        | ✔️         |
| async support     | ✔️        |          |          |          |            |          |           |

### Feature examples

- systems with early exit and failure
```rust
  use apecs::*;

  #[derive(Clone, Copy, Debug, Default, PartialEq)]
  struct Number(u32);

  fn demo_system(mut u32_number: ViewMut<Number>) -> Result<(), GraphError> {
      u32_number.0 += 1;
      if u32_number.0 == 3 {
          end()
      } else {
          ok()
      }
  }

  let mut world = World::default();
  world.add_subgraph(graph!(demo_system));
  world.run().unwrap();
  assert_eq!(Number(3), *world.get_resource::<Number>().unwrap());
  ```

- async futures can access world resources through a `Facade`
  - futures visit world resources with a closure. 
  - resources are acquired without lifetimes
  - plays well with any async runtime
```rust
  use apecs::*;

  #[derive(Clone, Copy, Debug, Default, PartialEq)]
  struct Number(u32);

  let mut world = World::default();
  let mut facade = world.facade();

  let task = smol::spawn(async move {
      loop {
          let i = facade
              .visit(|mut u32_number: ViewMut<Number>| {
                  u32_number.0 += 1;
                  u32_number.0
              })
              .await
              .unwrap();
          if i > 5 {
              break;
          }
      }
  });

  while !task.is_finished() {
      world.tick().unwrap();
      world.get_facade_schedule().unwrap().run().unwrap();
  }

  assert_eq!(Number(6), *world.get_resource::<Number>().unwrap());
```

- system data derive macros
```rust
  use apecs::*;

  #[derive(Edges)]
  struct MyData {
      entities: View<Entities>,
      u32_number: ViewMut<u32>,
  }

  let mut world = World::default();
  world
      .visit(|mut my_data: MyData| {
          *my_data.u32_number = 1;
      })
      .unwrap();
```

- system scheduling
  - compatible systems are placed in parallel batches (a batch is a group of systems
    that can run in parallel, ie they don't have conflicting borrows)
  - systems may depend on other systems running before or after
  - barriers
  ```rust
    use apecs::*;

    fn one(mut u32_number: ViewMut<u32>) -> Result<(), GraphError> {
        *u32_number += 1;
        end()
    }

    fn two(mut u32_number: ViewMut<u32>) -> Result<(), GraphError> {
        *u32_number += 1;
        end()
    }

    fn exit_on_three(mut f32_number: ViewMut<f32>) -> Result<(), GraphError> {
        *f32_number += 1.0;
        if *f32_number == 3.0 {
            end()
        } else {
            ok()
        }
    }

    fn lastly((u32_number, f32_number): (View<u32>, View<f32>)) -> Result<(), GraphError> {
        if *u32_number == 2 && *f32_number == 3.0 {
            end()
        } else {
            ok()
        }
    }

    let mut world = World::default();
    world.add_subgraph(
        graph!(
            // one should run before two
            one < two,
            // exit_on_three has no dependencies
            exit_on_three
        )
        // add a barrier
        .with_barrier()
        .with_subgraph(
            // all systems after a barrier run after the systems before a barrier
            graph!(lastly),
        ),
    );

    assert_eq!(
        vec![vec!["exit_on_three", "one"], vec!["two"], vec!["lastly"]],
        world.get_schedule_names()
    );

    world.tick().unwrap();

    assert_eq!(
        vec![vec!["exit_on_three"], vec!["lastly"]],
        world.get_schedule_names()
    );

    world.tick().unwrap();
    world.tick().unwrap();
    assert!(world.get_schedule_names().is_empty());
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

    fn create(mut entities: ViewMut<Entities>) -> Result<(), GraphError> {
        for mut entity in (0..100).map(|_| entities.create()) {
            entity.insert_bundle((0.0f32, 0u32, format!("{}:0", entity.id())));
        }
        end()
    }

    fn progress(q_f32s: Query<&mut f32>) -> Result<(), GraphError> {
        for f32 in q_f32s.query().iter_mut() {
            **f32 += 1.0;
        }
        ok()
    }

    fn sync(
        (q_others, mut tracker): (Query<(&f32, &mut String, &mut u32)>, ViewMut<MyTracker>),
    ) -> Result<(), GraphError> {
        for (f32, string, u32) in q_others.query().iter_mut() {
            if f32.was_modified_since(tracker.0) {
                **u32 = **f32 as u32;
                **string = format!("{}:{}", f32.id(), **u32);
            }
        }
        tracker.0 = apecs::current_iteration();
        ok()
    }

    // Entities and Components (which stores components) are default
    // resources
    let mut world = World::default();
    world.add_subgraph(graph!(
        create < progress < sync
    ));

    assert_eq!(
        vec![vec!["create"], vec!["progress"], vec!["sync"]],
        world.get_schedule_names()
    );

    world.tick().unwrap(); // entities are created, components applied lazily
    world.tick().unwrap(); // f32s are modified, u32s and strings are synced
    world.tick().unwrap(); // f32s are modified, u32s and strings are synced

    world
        .visit(|q_bundle: Query<(&f32, &u32, &String)>| {
            assert_eq!(
                (2.0f32, 2u32, "13:2".to_string()),
                q_bundle
                    .query()
                    .find_one(13)
                    .map(|(f, u, s)| (**f, **u, s.to_string()))
                    .unwrap()
            );
        })
        .unwrap();
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

    fn one(mut f32_number: ViewMut<F32>) -> Result<(), GraphError> {
        f32_number.0 += 1.0;
        ok()
    }

    fn two(f32_number: View<F32>) -> Result<(), GraphError> {
        println!("system two reads {}", f32_number.0);
        ok()
    }

    fn three(f32_number: View<F32>) -> Result<(), GraphError> {
        println!("system three reads {}", f32_number.0);
        ok()
    }

    world
        .add_subgraph(graph!(one, two, three))
        .with_parallelism(Parallelism::Automatic);

    world.tick().unwrap();
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
