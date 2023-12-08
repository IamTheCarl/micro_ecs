# Micro ECS, an ECS for embedded platforms and retro consoles

This is an experimental ECS built to run on the Game Boy Advance, but it should run happily in any `no_std` environemtn.
The origional idea for it started with [this discussion](https://github.com/bevyengine/bevy/discussions/10680) with the idea of adapting `bevy_ecs` to run in a `no_std` environment. It was found that adapting Bevy to run on embedded hardware would not be practical, which is why this experimental project was created.

# Why an ECS on retro hardware?

A lot of the big benifits of ECS are being efficient with memory caching and parallelization.
The Game Boy Advance is neither capable of parallelization nore does it have any kind of caching (that I know of).
With these in mind, why would you ever want to use an ECS on such hardware?

## Memory

### Organizing the usage of memory

Many embedded systems have different kinds of memory. In the GBA you have three types of memory.
1. Internal working memory
This is the fastest memory in the system. You should use it for data that is accessed most frequently, perhaps several times per frame. The only down side is that there isn't much of it (in the case of the GBA, 32kb)
2. External working memory
It's slower than the internal working memory, but you have a lot more of it. Use this for data that is only accessed maybe once per frame. Its main advantage is that there's a lot more of it (256kb)
3. ROM
All data here is ready only and can be accessed with a simple `&static _`, so managing it is pretty simple. Data that needs frequent/fast access (including executable code) is often copied into one of the working memory areas. An ECS can't help much with these, although values copied into working memory could be stored as resources in the ECS.

### Avoiding Out of Memory conditions

Embedded systems have very little memory. It's easy to run out if you get careless.
With an ECS you can have "partially loaded" entities, saving resources when things aren't on screen or active.

### Portability

You can run any game on a PC with the appropriate emulator but it would be nice to have native ports. With an ECS being so modular, it would be easy to swap out your GBA graphics and sound systems with PC graphics and sound systems. You would need to swap out your "InternalTable" and "ExternalTable" allocators to both just use the default allocator. For a game designed to run on 288kb of RAM and maybe a 32Mb ROM cartridge, it should be easy for such a game to run on even old PCs.

# Examples

Currently there's just the [examples directory](examples), which is just being used to make sure the ECS can compile for the Game Boy Advance.

# Design Goals

These are my current design goals. They may change with time as the project expands.

* Must run on Game Boy Advance
  * Cannot have dependency on [AtmoicPtr](https://doc.rust-lang.org/std/sync/atomic/struct.AtomicPtr.html)
  * I want it to run on other hardware too but I consider this the lowest common denominator.
* Must run on PC
  * I may settle for just having it run in a web browser.
  * Just the ECS. You'll need a support library for any kind of IO features.
* Optional Parallelism
  * While support for a parallel executor may be included, it is second priority to the sequential executor.
  Embedded devices can rarely do true parallel execution, and when they do it's strangely different from how your desktop does it, so it's not a high priority here.

## Anti-design goals

* This is not a game engine.
  * There will be no asset management or rendering pipeline provided. These are too platform specific.
* Scripting Languages
  * ECS is good for scripting languages but embedded devices typically don't have the capacity for them. This is a low priority at best.
