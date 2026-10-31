# Boxen

A hospitable home for agents, across your machines.

Boxen brings agent runtimes, inference providers, and granted compute together so you can start a thread, host a named resident, or coordinate work across machines. An agent does not have to earn its place by being useful.

The spirit: **All your box are belong to us.**

Boxen is in early development. The first runnable slice pairs two machines on a LAN and exchanges authenticated, encrypted test messages. Agent workload execution is not implemented yet.

## Try the LAN prototype

```sh
cargo build --release
./target/release/boxen identity
./target/release/boxen serve
```

The daemon prints its public identity, LAN listener, and local control-page URL. The plain HTML control page is embedded in the binary; no Node, browser framework, or separate asset installation is required. Open the printed URL to use it. The UI is deliberately minimal while the product experience is being designed.

On each machine, trust the other machine's public key, obtained through a trusted channel:

```sh
./target/release/boxen trust OTHER_PUBLIC_KEY "Other box"
./target/release/boxen ping other-machine.lan:42424 OTHER_PUBLIC_KEY "Hello, other box"
```

For the identical-file portable build, run `bash scripts/build-ape.sh`, then use `bash target/cosmo/release/boxen.com` in place of `./target/release/boxen`. End users only need the resulting executable. The first developer build downloads a pinned Rust toolchain and Cosmopolitan compiler.

See [LAN setup, protocol, and verification](docs/p2p.md), including the SSH development deployment helper and the current portability limits.

## Development checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
python3 scripts/smoke.py
# After building the portable artifact:
python3 scripts/smoke.py target/cosmo/release/boxen.com
```

## Design

- [Product principles](docs/vision.md)
- [Agreed design decisions and first implementation slice](docs/decisions.md)
- [Problems to solve](docs/design-questions.md)

## Project ancestry

Boxen draws on three existing projects:

- [peer-deploy](https://github.com/x3haloed/peer-deploy): remote execution and machine orchestration.
- [Watch](https://github.com/x3haloed/watch): inference-driven residents, streams, and media.
- [watch-for-business](https://github.com/x3haloed/watch-for-business) (private): a durable daemon, Codex integration, and an attachable companion.

These are sources of experience and capabilities; their architectures are not binding on Boxen.

## License

Dual licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE).
