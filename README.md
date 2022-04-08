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
[x] fetch resources from the world asyncronously
[x] async systems, ie systems that end and/or change over time (for scenes, stories, etc)
[x] sync systems with failure
[x] futures
[x] system data derive macros
[x] efficient (in space or iteration) component storages
[x] joins / queries
[x] parallel joins / queries (inner parallelism)
[x] parallel system scheduling (outer parallelism)
[x] tracked storage (components created, updated, modified)
[x] insert plugins/bundles (groups of systems and resources)
