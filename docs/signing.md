# Signing

Today one key signs Ooblin: our own, for the updater. An installed Ooblin only takes an update that this key signed. Apple and Microsoft do not sign it yet, see the end.

## The updater's key

The public half is `plugins.updater.pubkey` in `crates/ooblin/tauri.conf.json`, so every Ooblin we ship carries it. The private half is the `TAURI_SIGNING_PRIVATE_KEY` secret of the repository, which only the signing step of the release job gets.

We made it once, like this, and a new one is made the same way:

1. Generate the pair outside the repository. It asks for a password, which may be empty:

   ```sh
   cargo tauri signer generate -w ~/.tauri/ooblin-updater.key
   ```

2. Put the content of `~/.tauri/ooblin-updater.key.pub` in `plugins.updater.pubkey`.
3. Give CI the private half, and its password if it has one:

   ```sh
   gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/ooblin-updater.key
   gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD
   ```

4. Keep a copy of `~/.tauri/ooblin-updater.key` and its password somewhere safe, a password manager for example.

IMPORTANT: An installed Ooblin trusts the public key it was built with and no other, so a new key only reaches it through a release signed with the old one. Lose the private key and everyone has to install Ooblin again by hand. Leak it and anyone can sign an update for us!

The release job signs each bundle as you would by hand, which is also how to check a key:

```sh
cargo tauri signer sign -f ~/.tauri/ooblin-updater.key --app-version 0.2.0 Ooblin_0.2.0_universal.app.tar.gz
```

## Apple and Microsoft

TODO: Neither signs Ooblin yet, so macOS asks once to allow it (Open Anyway) and Windows shows SmartScreen.

- macOS wants a "Developer ID Application" certificate of the Apple Developer Program (99 dollars a year), and Apple's notarization. `cargo tauri build` does both when the build job has `APPLE_CERTIFICATE` (the `.p12`, in base64), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` and an App Store Connect API key in `APPLE_API_ISSUER`, `APPLE_API_KEY` and `APPLE_API_KEY_PATH` ([Tauri's guide](https://v2.tauri.app/distribute/sign/macos/)). Then the ad-hoc identity (`-`) of the macOS build in `ci.yml` goes, and so does the caveat of the cask.
- Windows wants a code signing certificate, or Azure's signing service ([Tauri's guide](https://v2.tauri.app/distribute/sign/windows/)).
