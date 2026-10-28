# Vendored crates

- `gpui-component` 0.8.1 (Apache-2.0, LICENSE-APACHE inside), unchanged
  except `src/window_border.rs`: `SHADOW_SIZE` is 0 on Linux too, so the
  main window fills its surface instead of showing a 20 px band where a
  transparent shadow can't be drawn. Search for "Mostalgia:" for the change.
  Drop this patch (`[patch.crates-io]` in the root `Cargo.toml`) when
  upgrading gpui-kit, and check the window frame again.
