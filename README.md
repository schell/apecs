# apecs
**A**syncronous **P**arallel **E**ntity **C**omponent **S**ystem

`apecs` is an entity-component system that supports traditional syncronous systems as well as
asyncronous systems that can evolve over time. This makes it great for quick game prototypes
and DIY engines.

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
- support for general futures
- fetch data (system data) derive macros
- system scheduling
  - systems may depend on other systems running first
  - barriers
- component storage
  - optimized for space and iteration time as archetypes
  - queries with "maybe" and "without" semantics
  - queries can find a single entity without iteration and filtering
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

## Benchmarks
```
```
