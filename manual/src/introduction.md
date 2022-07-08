# Introduction
Welcome to APECS!

APECS is an ECS library written in Rust.

## What is an ECS library?

ECS stands for Entity Component System.

* mostly about program architecture
* breaks up the program into entities+components and then systems/functions
  that operate on them separately
* all state is stored in the "world" and a subset is accessed by each system

### entities & components

* entities are simple ids, ie `usize`

* a component is one type of data
  - `struct Postion { x: f32, y: f32 }`
  - `struct Velocity { x: f32, y: f32 }`
  - `struct Acceleration { x: f32, y: f32 }`

* joins - like a database, join columns of components that share the same entity into an
  iterator:
  ```rust,ignore
  for (camera, may_name, layers, stage) in (
      &mut self.cameras,
      (&self.names).maybe(),
      (&self.layers).maybe(),
      &self.stages,
  ).join() {
      //use `camera` mutably etc...
  }
  ```
* parallel joins (inner parallelism)

#### Why not use an in-memory database?

* iteration speed
  - special entity+component stores in contiguous memory
* sharing access
* you would have to roll-your-own systems, scheduling and data sharing (more on that later)

### systems

* at its core, a system is just a function that mutates the world

The "system" in ECS is doing a _lot_ of heavy lifting. We're really talking about _running_ systems,
and everything it takes to support running many systems efficiently.

* many ECS libraries provide a data sharing setup to support running systems
* each datum accessed by a system is called a "resource"
* component columns can be a "resource", therefore **mutable** queries of disjoint columns are separate "resources" and can
  run in parallel
* queries without mutation can be run in parallel even with overlapping columns
* system scheduling (outer parallelism)

## Why use an ECS library?

* supports lots and lots of loosely related objects (entities+components)
* supports complex data sharing without locks
* very flexible, easier to add features and change systems
* coming from games it offers very predictable performance and it's **easy
  to profile**

### More on-topic reading
* [ECS FAQ](https://github.com/SanderMertens/ecs-faq)
