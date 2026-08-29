# Roadmap

The v1 goal is a dependable local core: automatic Checkpoints, isolated Private Layers, linear verified Publish, explicit conflicts, provenance intake, retention, and recovery without Git.

Post-v1 work is evidence-driven:

1. Validate Linux and Windows through native release-binary gauntlets, then publish supported artifacts.
2. Replace polling with native filesystem event sources while retaining full-scan reconciliation.
3. Add Linux reflink materialization and improve cross-filesystem diagnostics.
4. Add bounded queue and reconciliation metrics to `doctor` without remote telemetry.
5. Optimize history and large-tree scans from benchmark profiles.
6. Evaluate a virtual Layer filesystem only if ordinary directory materialization is a measured product bottleneck.
7. Define a remote transport protocol only after the local storage and contribution formats have operational evidence.

A custom database, universal semantic merge, agent launcher, deployment system, and hostile-code sandbox are not implied roadmap commitments.
