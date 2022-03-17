# apecs
**A**syncronous **P**arallel **E**ntity **C**omponent **S**ystem

> NOTE: not yet parallel

`apecs` is an entity-component system that spawns asyncronous and possibly
short-lived systems.

## Goals
* ease of use - the API is very small and friendly, errors are well explained
* speed - performance should be competitive with inspirational ECS libraries ie `specs`, `legion`, `shipyard`

## TODO
[x] fetch resources from the world asyncronously
[x] variable systems, ie systems that exit early and change over time
[x] support for general futures
[x] system data derive macros
[x] joins / queries
[ ] efficient component storages
[ ] parallel system scheduling
[ ] plugins/bundles
