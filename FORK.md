# This fork

`slyedoc/jackdaw`, forked from `jbuehler23/jackdaw`. Three branches, each based on the
one above it, so a change only ever flows DOWN:

    main        tracks upstream. Merge from jbuehler23 here and nowhere else.
      |
    bevy-main   the bevy 0.20-dev port: upstream's editor on the bevy revision aurora
      |         is built against, still on wgpu/bevy_render. One copy of bevy in the
      |         whole graph -- see the `[patch]` table in Cargo.toml and the commit
      |         message on 5d5c0dc for why the manifest names upstream git urls.
      |
    aurora      swaps bevy_render for `bevy_aurora`, the pure-Vulkan ray tracer.

To take upstream work: merge into `main`, then `main` -> `bevy-main`, then
`bevy-main` -> `aurora`. Never merge upstream straight into a lower branch; the
patch table and the render swap would fight it every time.

`origin/bevy-main` used to hold an unrelated 0.19-dev attempt of upstream's
(`8c689f6`, reachable by sha in the fork network). It was replaced, not merged.

## Side-by-side checkouts

The patch table redirects every bevy crate to `../bevy`, so this repo expects:

    /mnt/code/f/bevy        slyedoc/bevy, branch `aurora`
    /mnt/code/f/jackdaw     this repo
    /mnt/code/p/aurora      slyedoc/aurora (crate `bevy_aurora`)

## Build

    cargo run -r --features dylib

from this repo. The first build takes about half an hour.
