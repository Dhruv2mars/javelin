# TypeScript protocol (v2)

The one TypeScript implementation of the node protocol, shared by TypeScript clients and
Workers. It mirrors `crates/javelin-protocol/src/codec.rs` for the read messages
(`Get`/`Objects`, `Resolve`/`Resolved`, `Changeset`/`ChangesetPart`,
`Attribution`/`Attributed`, `Error`) and adds the object formats a reader needs: object and
version IDs, tree chunks and canonical changesets.

- `codec.ts`: message bodies, as sent over `POST /v2/r/{repository}`.
- `objects.ts`: hashing, tree chunk and changeset decoding.
- `client.ts`: `NodeClient`, which checks every object against its ID.
- `tree.ts`: directory listing and path lookup.

It has no dependencies and uses only Web APIs (`fetch`, `crypto.subtle`), so it runs in
browsers, Bun and Workers. Import it by relative path. Its tests run with the forge
(`apps/forge/test`) against a real `javelin-node`.
