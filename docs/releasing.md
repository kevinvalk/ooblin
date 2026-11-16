# Releasing

A tag is the release, and its name is the version. Nothing else in the repository holds the version: `Cargo.toml` says 0.0.0, which is also what a build from source says it is.

```sh
git tag -m "Ooblin 0.2.0" v0.2.0
git push origin v0.2.0
```

On that tag CI (`.github/workflows/ci.yml`) builds every bundle at that version, signs them for the updater (see [signing](signing.md)), publishes the release with its `latest.json`, and commits the version and checksum of the new `.dmg` to the Homebrew cask (`Casks/ooblin.rb`) on main. So pull after a release.

- A tag is `vX.Y.Z` and nothing else, as MSI takes only a number after the version and RPM no hyphen at all. CI refuses any other.
- Every push to main replaces the "Unstable" prerelease, versioned as the last tag plus the commits since (`0.2.0+5`). The updater offers it the next release but not the one it was built on, and never offers Unstable itself, as GitHub's latest release is never a prerelease.
- A run that fails before the release is published leaves only the tag behind. Fix it and tag the next patch, or delete the tag (`git push --delete origin v0.2.0`) and push it again.
- The 200 rounds of garbled devices (`garble-and-read`) do not hold a release up, but their failure still fails the run.
