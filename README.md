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
- async systems, ie systems that end and/or change over time (for scenes, stories, etc)
  - fetch resources from the world asyncronously
  - resources are acquired without lifetimes
- support for async futures
- fetch data (system data) derive macros
- system scheduling
  - systems may depend on other systems running first
  - barriers
- component storage
  - optimized for space and iteration time as archetypes
  - queries with "maybe" and "without" semantics
  - queries can find a single entity without iteration or filtering
  - add and modified time tracking
  - parallel queries (inner parallelism)
- parallel system scheduling (outer parallelism)
- parallel execution of async futures
- plugins (groups of systems, resources and sub-plugins)
- configurable parallelism (can be automatic or a requested number of threads, including 1)
- fully compatible with wasm

## Roadmap
- your ideas go here

## Tests
```
cargo test
wasm-pack test --firefox crates/apecs
```

I like firefox, but you can use different browsers for the wasm tests. For the most part they're there
just to make sure apecs works on wasm.

## Benchmarks
The `apecs` benchmarks measure itself against my favorite ECS libs:
`specs`, `bevy`, `hecs`, `legion`, `shipyard` and `planck_ecs`.

```
cargo bench -p benchmarks
```

# Caveats
`apecs` uses generic associated types. This means it can only be compiled with nightly.
