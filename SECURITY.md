# Security

Please report vulnerabilities privately through GitHub: the repository's **Security** tab, then
**Report a vulnerability**. Do not open a public issue for a security problem.

In scope:

- `crates/ignot-core`: any way to make verification pass for an exam whose sealed file, questions, replies or
  scores were changed; any way to open a seal before its drand round or with something that is not that round's
  signature; any input that makes the engine crash, hang or use unbounded memory.
- `crates/ignot-cli`: any path that prints or writes the OpenRouter key, sends it anywhere but the provider, or
  writes outside the folder a command was given.
- `crates/ignot-wasm` and `site`: any published file that can run script in a reader's browser or make the
  in-page verifier report something the command line would not.
- `contracts`: any way to record a seal after its round opened, publish results before it opened, overwrite a
  seal or a root, make `StumpBox` hold tokens, or accept a submission without the burn.

Out of scope: the drand network itself, OpenRouter, and model providers.

What the engine assumes: SHA-256, the BLS12-381 pairing as implemented by arkworks, the age file format as
implemented by the `age` crate, and that fewer than a threshold of the League of Entropy's nodes collude to
sign a round early.
