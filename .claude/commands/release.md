---
description: Prépare, commit et build une release (bump de version, tag, push déclenchant la GitHub Action)
argument-hint: "[patch|minor|major|X.Y.Z]"
allowed-tools: Bash(git *), Bash(cargo *), Read, Edit
---

Version demandée : `$ARGUMENTS` (défaut : `patch`).

1. `git status --porcelain` doit être vide et la branche `main`, sinon stop et le signaler.
2. Lire la version actuelle dans `Cargo.toml`, calculer la nouvelle (semver ; `patch`/`minor`/`major` ou version explicite) et la modifier dans `[package]`.
3. `cargo build --release` (met aussi `Cargo.lock` à jour). Corriger les erreurs avant de continuer.
4. `git add Cargo.toml Cargo.lock && git commit -m "release vX.Y.Z"` puis `git tag vX.Y.Z`.
5. Demander confirmation, puis `git push origin main vX.Y.Z` : le tag lance `.github/workflows/release.yml` (builds Linux + Windows, publication de la release GitHub avec les binaires).
6. Donner l'URL du run : `https://github.com/buzz69/mco_dash/actions`.
