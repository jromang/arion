# arion-web frontend sources

Phase W1 ships a hand-written `dist/index.html` — no build step.

Starting at phase W2 this directory will hold a Svelte + Vite
project. The build output will live in `dist/` and is embedded into
the Rust binary via `rust-embed`. `cargo build` never invokes npm —
run `npm run build` here manually when the frontend changes.
