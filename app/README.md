# Clipper App

Flutter client for Clipper encrypted clipboard and file sync.

## Android

The Android app talks to the Clipper server directly through the shared Rust
client engine. The emulator default server URL is `http://10.0.2.2:8787`, which
maps to the host machine.

Run through the repository flake:

```sh
nix develop --command bash -c 'cd app && flutter run -d android'
```

If direnv has already loaded the flake shell, `flutter run -d android` from
this directory is equivalent.

Use an HTTPS server URL for physical devices unless the server is exposed
through a trusted local development setup.
