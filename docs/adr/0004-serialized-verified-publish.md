# ADR 0004: Serialized verified Publish

Status: accepted

Publish uses a fair target-scoped OS lease. It reconciles, refreshes, builds and verifies an isolated candidate, persists objects, then uses one short SQLite transaction to append accepted records and update the target pointer.

