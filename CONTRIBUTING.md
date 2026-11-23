# Contributing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --include-ignored     # no network; the ignored test needs python3
forge test --root contracts
cd site && npm ci && npm run build               # needs wasm-bindgen-cli 0.2.130
```

Rules:

- Source, comments and docs are English and ASCII only.
- No secrets in the repository. The runner reads `OPENROUTER_API_KEY` from the environment and never prints it.
- `ignot-core` reads no clock, no file and no network: whoever runs it passes in what it needs. That is what
  lets the same code run in a browser, and it is enforced by `crates/ignot-core/clippy.toml`.
- No `unsafe`. Outside tests, no `unwrap`, `expect` or `panic!`: anything that can go wrong on input from
  outside is an error value. The workspace lints enforce both.
- Nothing that is hashed holds a fraction. Money is whole billionths of a dollar.
- A new kind of question needs a test that solves it again from the text of the question, with code that does
  not share the generator's logic.
- Changing what a generator makes for a given seed means raising `GENERATOR_VERSION` in
  `crates/ignot-core/src/lib.rs`, so that older exams keep pointing at the commit that can make them again.
- Commit messages say why the change is needed.
