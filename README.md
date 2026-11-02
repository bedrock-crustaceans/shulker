# shulker

Checks Minecraft Bedrock packs and add-ons and prints what the game would complain about, in the
style of `rustc`: the file and line, the marked span, and a `help:` saying what to change. Built
on [`bedrock_addon`](https://github.com/bedrock-crustaceans/bedrock-rs/tree/feat/addon/crates/addon/addon)
and its std driver.

```
error[InvalidType]: wrong JSON type
 --> packs/bp/blocks/lamp.json:7:29
  |
7 |       "minecraft:friction": "slippery"
  |                             ^^^^^^^^^^ wrong type
  |
  = note: at `minecraft:block/components/minecraft:friction`
  = note: in pack `Ruby BP`
  = help: change `minecraft:friction` to the JSON type it expects (string, number, object, …)
```

## Usage

```sh
shulker <PATH>... [--min-severity debug|info|warning|error] [--no-link] [--no-lints] [--no-vanilla] [-q]
```

Each path is a pack folder, a folder of packs, a world folder or an archive (`.mcpack`,
`.mcaddon`, `.mcworld`, `.zip`). Everything given is checked together, so a pack that depends on
another one is checked by passing both:

```sh
shulker "Ruby Behavior.mcpack" "Ruby Resource.mcpack"
```

A world uses its own pack lists; anything else is checked with every pack switched on. The exit
code is 1 when anything is an error.

## Vanilla files

The game's own packs and the definitions outside them are embedded in the binary, so a pack is
checked on top of vanilla as the game loads it: references to vanilla textures, geometries,
UI templates and so on resolve, and findings in vanilla's own files are not shown.
`shulker --version` names the install they came from.

They are baked from a local game install (the folder holding `behavior_packs/` and
`resource_packs/`) into `vanilla/vanilla.zst`, which is not committed:

```sh
cargo run --release --bin bake-vanilla -- \
    <game install>/data
cargo build --release
```

Everything is kept byte for byte, brarchives and MCB files included, except large audio and
images, which are cut to their first 4 KiB (inside brarchives too): the checks read only their
headers. A build without the bundle works too and checks packs without vanilla.

## Building

`bedrock-rs` comes from the `feat/addon` branch on GitHub, at the commit in `Cargo.lock`;
`cargo update -p bedrock` moves it to the branch's head. To build against a local
checkout instead, patch it in `.cargo/config.toml`:

```toml
[patch."https://github.com/bedrock-crustaceans/bedrock-rs"]
bedrock = { path = "../bedrock-rs/bedrock" }
```

Keep `smallvec` at `2.0.0-beta.1`: a newer beta breaks `bedrock_addon_support`. After a plain
`cargo update`, pin it back with
`cargo update -p smallvec@2.0.0-beta.4 --precise 2.0.0-beta.1` (or whichever beta it moved to).
