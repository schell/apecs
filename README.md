# apecs
**A**syncronous **P**arallel **E**ntity **C**omponent **S**ystem

`apecs` is an entity-component system that supports syncronous and asyncronous, possibly
short-lived systems.

## Goals
* productivity
* flexibility
* don't be too magical / explicit API (within reason)
* only pay for what you use
* well rounded performance, competitive with inspirational ECS libraries
  - like `specs`, `bevy_ecs`, `hecs`, `legion`, `shipyard`

## Features
- fetch resources from the world asyncronously
- async systems, ie systems that end and/or change over time (for scenes, stories, etc)
- sync systems with failure
- futures
- system data derive macros
- efficient (in space or iteration) component storages
- joins / queries
- parallel joins / queries (inner parallelism)
- parallel system scheduling (outer parallelism)
- tracked storage (components created, updated, modified)
- insert plugins/bundles (groups of systems and resources)
- schedule: system dependencies
- schedule: barriers

## Roadmap
- schedule: local thread systems (maybe not necessary?)
- wasm
- make range storage faster
- archtypal storage (why should an ecs only have one or the other?)
