# Native repository foundation

## Initial baseline

Before implementation, `rtk cargo test` passed with zero tests and
`rtk cargo run -- init` printed `Hello, world!` without creating a repository.
`rtk cargo test --test scaffold` then passed one process test that asserted those
exact stdout bytes and an empty stderr. This baseline is captured in the scaffold
commit; the test will become a native initialization test after implementation.
The host uses rustc 1.93.0 and cargo 1.93.0 on macOS.

## Bounded outcome

This PR proves immutable native content, manifests, initial World, ordered
history, and compare-and-advance publication metadata. It adds init, history,
and integrity inspection commands. Layer creation, checkpointing, sparse
filesystem access, Stack, refresh, permissions, policies, and complete Layer
Publish are later work. Runtime and tests never invoke or interpret Git.
