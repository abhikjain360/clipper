{
  description = "Clipper development environment";

  inputs = {
    # Use unstable until the latest stable Nix channel has Dart >= 3.11.1.
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      rustNightlyToolchain = "nightly-2026-05-24";
      androidRustTargets = [
        "aarch64-linux-android"
        "armv7-linux-androideabi"
        "i686-linux-android"
        "x86_64-linux-android"
      ];
      wasmRustTarget = "wasm32-unknown-unknown";
      wasmPackRustflags = "-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=1073741824 -C link-arg=--export=__heap_base -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base";
      stableRustTargets = androidRustTargets ++ [ wasmRustTarget ];
      stableRustTargetsString = nixpkgs.lib.concatStringsSep " " stableRustTargets;
      mkPkgs = system: import nixpkgs { inherit system; };
      mkCommandScripts =
        pkgs:
        let
          commonRuntimeInputs = with pkgs; [
            coreutils
            dart
            flutter
            flutter_rust_bridge_codegen
            git
            nixfmt
            python3
            rustup
            wasm-pack
          ];
          repoRoot = ''
            repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
            cd "$repo_root"
          '';
          rustupSetup = ''
            export PATH="$PATH:$HOME/.cargo/bin"
            export CLIPPER_RUST_NIGHTLY="${rustNightlyToolchain}"
            export CLIPPER_WASM_PACK_RUSTFLAGS="${wasmPackRustflags}"

            ensure_rustup_toolchain() {
              local toolchain="$1"
              local components="$2"
              local targets="$3"
              local component_args=()
              local target_args=()

              IFS=',' read -r -a component_args <<< "$components"
              read -r -a target_args <<< "$targets"

              if ! rustup toolchain list | grep -Eq "^$toolchain(-| )"; then
                rustup toolchain install "$toolchain" \
                  --profile minimal \
                  --no-self-update
              fi

              rustup component add --toolchain "$toolchain" "''${component_args[@]}"
              if [ "''${#target_args[@]}" -gt 0 ]; then
                rustup target add --toolchain "$toolchain" "''${target_args[@]}"
              fi
            }

            ensure_rustup_toolchain stable "rustfmt,clippy,rust-src" "${stableRustTargetsString}"
            ensure_rustup_toolchain "$CLIPPER_RUST_NIGHTLY" "rustfmt,rust-src" "${wasmRustTarget}"
          '';
        in
        {
          fmt = pkgs.writeShellApplication {
            name = "clipper-fmt";
            runtimeInputs = commonRuntimeInputs;
            text = ''
              ${repoRoot}
              ${rustupSetup}

              nixfmt flake.nix
              rustup run "$CLIPPER_RUST_NIGHTLY" cargo fmt --all
              dart format app/lib app/test app/integration_test app/test_driver
            '';
          };

          rustfmt = pkgs.writeShellApplication {
            name = "clipper-rustfmt";
            runtimeInputs = commonRuntimeInputs;
            text = ''
              ${repoRoot}
              ${rustupSetup}

              rustup run "$CLIPPER_RUST_NIGHTLY" cargo fmt --all "$@"
            '';
          };

          wasm-check = pkgs.writeShellApplication {
            name = "clipper-wasm-check";
            runtimeInputs = commonRuntimeInputs;
            text = ''
              ${repoRoot}
              ${rustupSetup}

              cargo check -p rust_lib_clipper_app --target ${wasmRustTarget} "$@"
            '';
          };

          frb-generate = pkgs.writeShellApplication {
            name = "clipper-frb-generate";
            runtimeInputs = commonRuntimeInputs;
            text = ''
              ${repoRoot}

              export FLUTTER_ROOT="${pkgs.flutter}"
              cd app
              flutter_rust_bridge_codegen generate "$@"
            '';
          };

          frb-build-web = pkgs.writeShellApplication {
            name = "clipper-frb-build-web";
            runtimeInputs = commonRuntimeInputs;
            text = ''
              ${repoRoot}
              ${rustupSetup}

              export FLUTTER_ROOT="${pkgs.flutter}"
              cd app
              flutter_rust_bridge_codegen build-web \
                --wasm-pack-rustup-toolchain "$CLIPPER_RUST_NIGHTLY" \
                --wasm-pack-rustflags "$CLIPPER_WASM_PACK_RUSTFLAGS" \
                "$@"
            '';
          };

          web-build = pkgs.writeShellApplication {
            name = "clipper-web-build";
            runtimeInputs = commonRuntimeInputs;
            text = ''
              ${repoRoot}
              ${rustupSetup}

              export FLUTTER_ROOT="${pkgs.flutter}"
              cd app
              flutter pub get
              flutter_rust_bridge_codegen build-web \
                --wasm-pack-rustup-toolchain "$CLIPPER_RUST_NIGHTLY" \
                --wasm-pack-rustflags "$CLIPPER_WASM_PACK_RUSTFLAGS"
              flutter build web --no-pub --no-wasm-dry-run "$@"
            '';
          };

          web-serve = pkgs.writeShellApplication {
            name = "clipper-web-serve";
            runtimeInputs = commonRuntimeInputs;
            text = ''
              ${repoRoot}

              root="''${CLIPPER_WEB_ROOT:-app/build/web}"
              preferred_port="''${1:-53880}"

              if [ ! -f "$root/index.html" ]; then
                echo "missing $root/index.html; run nix run .#web-build first" >&2
                exit 1
              fi

              python3 - "$root" "$preferred_port" <<'PY'
              import errno
              import functools
              import http.server
              import socket
              import socketserver
              import sys

              root = sys.argv[1]
              preferred_port = int(sys.argv[2])


              class Handler(http.server.SimpleHTTPRequestHandler):
                  def end_headers(self):
                      self.send_header("Cross-Origin-Opener-Policy", "same-origin")
                      self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
                      self.send_header("Cross-Origin-Resource-Policy", "same-origin")
                      super().end_headers()

                  def guess_type(self, path):
                      if path.endswith(".wasm"):
                          return "application/wasm"
                      return super().guess_type(path)


              class Server(socketserver.TCPServer):
                  allow_reuse_address = True


              handler = functools.partial(Handler, directory=root)
              server = None
              last_error = None

              for port in range(preferred_port, preferred_port + 100):
                  try:
                      server = Server(("127.0.0.1", port), handler)
                      break
                  except OSError as error:
                      if error.errno != errno.EADDRINUSE:
                          raise
                      last_error = error

              if server is None:
                  raise last_error or RuntimeError("could not bind local web server")

              with server:
                  _, port = server.server_address
                  print(f"Serving {root} at http://127.0.0.1:{port}/")
                  print("Sending COOP/COEP headers required by the Rust wasm worker.")
                  sys.stdout.flush()
                  server.serve_forever()
              PY
            '';
          };
        };
      mkApp = drv: program: description: {
        type = "app";
        program = "${drv}/bin/${program}";
        meta.description = description;
      };
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          lib = pkgs.lib;
          darwinInputs = lib.optionals pkgs.stdenv.isDarwin [
            pkgs.apple-sdk_15
            pkgs.libiconv
          ];
        in
        {
          default = pkgs.mkShell {
            packages =
              (with pkgs; [
                cargo-edit
                cmake
                cocoapods
                dart
                deno
                flutter
                flutter_rust_bridge_codegen
                jdk17
                llvmPackages.clang
                llvmPackages.libclang
                ninja
                nixfmt
                openssl
                pkg-config
                python3
                rust-analyzer
                rustup
                sea-orm-cli
                sqlite
                wasm-pack
              ])
              ++ darwinInputs;

            env = {
              CLIPPER_RUST_NIGHTLY = rustNightlyToolchain;
              CLIPPER_WASM_PACK_RUSTFLAGS = wasmPackRustflags;
              COCOAPODS_DISABLE_STATS = "1";
              FLUTTER_ROOT = "${pkgs.flutter}";
              JAVA_HOME = "${pkgs.jdk17.home}";
              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              RUST_BACKTRACE = "1";
            };

            shellHook = ''
              export PATH="$PATH:$HOME/.cargo/bin"

              rust_targets="${stableRustTargetsString}"
              wasm_target="${wasmRustTarget}"
              if command -v rustup >/dev/null 2>&1; then
                ensure_rustup_toolchain() {
                  toolchain="$1"
                  components="$2"
                  targets="$3"
                  component_args="$(printf '%s' "$components" | tr ',' ' ')"

                  if ! rustup toolchain list | grep -Eq "^$toolchain(-| )"; then
                    rustup toolchain install "$toolchain" \
                      --profile minimal \
                      --no-self-update
                  fi

                  rustup component add --toolchain "$toolchain" $component_args >/dev/null 2>&1 \
                    || echo "warning: failed to install Rust components for $toolchain"
                  if [ -n "$targets" ]; then
                    rustup target add --toolchain "$toolchain" $targets >/dev/null 2>&1 \
                      || echo "warning: failed to install Rust targets for $toolchain"
                  fi
                }

                ensure_rustup_toolchain stable "rustfmt,clippy,rust-src" "$rust_targets"
                ensure_rustup_toolchain "$CLIPPER_RUST_NIGHTLY" "rustfmt,rust-src" "$wasm_target"

                export RUST_SRC_PATH="$(rustup run stable rustc --print sysroot)/lib/rustlib/src/rust/library"
              fi

              if [ -z "''${ANDROID_HOME:-}" ]; then
                if [ -d "$HOME/Library/Android/sdk" ]; then
                  export ANDROID_HOME="$HOME/Library/Android/sdk"
                elif [ -d "$HOME/Android/Sdk" ]; then
                  export ANDROID_HOME="$HOME/Android/Sdk"
                elif [ -d "$HOME/.local/share/Android/Sdk" ]; then
                  export ANDROID_HOME="$HOME/.local/share/Android/Sdk"
                fi
              fi

              if [ -n "''${ANDROID_HOME:-}" ]; then
                export ANDROID_SDK_ROOT="''${ANDROID_SDK_ROOT:-$ANDROID_HOME}"
                export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"

                if [ -z "''${ANDROID_NDK_HOME:-}" ] && [ -d "$ANDROID_HOME/ndk" ]; then
                  latest_ndk="$(
                    find "$ANDROID_HOME/ndk" -mindepth 1 -maxdepth 1 -type d 2>/dev/null \
                      | awk -F/ '{ print $NF " " $0 }' \
                      | sort -t. -k1,1n -k2,2n -k3,3n \
                      | awk '{ print $2 }' \
                      | tail -n 1
                  )"
                  if [ -n "$latest_ndk" ]; then
                    export ANDROID_NDK_HOME="$latest_ndk"
                  fi
                fi
              fi

              if [ -n "''${ANDROID_NDK_HOME:-}" ]; then
                export ANDROID_NDK_ROOT="''${ANDROID_NDK_ROOT:-$ANDROID_NDK_HOME}"
                case "$(uname -s)" in
                  Darwin) ndk_host="darwin-x86_64" ;;
                  Linux) ndk_host="linux-x86_64" ;;
                  *) ndk_host="" ;;
                esac

                ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$ndk_host/bin"
                if [ -n "$ndk_host" ] && [ -d "$ndk_bin" ]; then
                  export PATH="$ndk_bin:$PATH"

                  export CC_aarch64_linux_android="$ndk_bin/aarch64-linux-android21-clang"
                  export CXX_aarch64_linux_android="$ndk_bin/aarch64-linux-android21-clang++"
                  export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_bin/aarch64-linux-android21-clang"

                  export CC_armv7_linux_androideabi="$ndk_bin/armv7a-linux-androideabi21-clang"
                  export CXX_armv7_linux_androideabi="$ndk_bin/armv7a-linux-androideabi21-clang++"
                  export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$ndk_bin/armv7a-linux-androideabi21-clang"

                  export CC_i686_linux_android="$ndk_bin/i686-linux-android21-clang"
                  export CXX_i686_linux_android="$ndk_bin/i686-linux-android21-clang++"
                  export CARGO_TARGET_I686_LINUX_ANDROID_LINKER="$ndk_bin/i686-linux-android21-clang"

                  export CC_x86_64_linux_android="$ndk_bin/x86_64-linux-android21-clang"
                  export CXX_x86_64_linux_android="$ndk_bin/x86_64-linux-android21-clang++"
                  export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$ndk_bin/x86_64-linux-android21-clang"

                  export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
                  export AR_armv7_linux_androideabi="$ndk_bin/llvm-ar"
                  export AR_i686_linux_android="$ndk_bin/llvm-ar"
                  export AR_x86_64_linux_android="$ndk_bin/llvm-ar"
                  export RANLIB_aarch64_linux_android="$ndk_bin/llvm-ranlib"
                  export RANLIB_armv7_linux_androideabi="$ndk_bin/llvm-ranlib"
                  export RANLIB_i686_linux_android="$ndk_bin/llvm-ranlib"
                  export RANLIB_x86_64_linux_android="$ndk_bin/llvm-ranlib"
                fi
              fi

              echo "clipper dev shell"
              echo "  rust: $(rustup run stable rustc --version)"
              echo "  rust nightly: $(rustup run "$CLIPPER_RUST_NIGHTLY" rustc --version)"
              echo "  flutter: $(flutter --version | sed -n '1p')"
              echo "  sea-orm-cli: $(sea-orm-cli --version)"
              if [ -n "''${ANDROID_HOME:-}" ]; then
                echo "  android sdk: $ANDROID_HOME"
              fi
              if [ -n "''${ANDROID_NDK_HOME:-}" ]; then
                echo "  android ndk: $ANDROID_NDK_HOME"
              else
                echo "  android ndk: not configured"
              fi
              echo "host Xcode, Android SDK/NDK installs, and emulators remain platform setup"
            '';
          };
        }
      );

      packages = forAllSystems (
        system:
        let
          scripts = mkCommandScripts (mkPkgs system);
        in
        scripts // { default = scripts.fmt; }
      );

      apps = forAllSystems (
        system:
        let
          scripts = mkCommandScripts (mkPkgs system);
        in
        {
          default = mkApp scripts.fmt "clipper-fmt" "Format all Clipper sources";
          fmt = mkApp scripts.fmt "clipper-fmt" "Format all Clipper sources";
          rustfmt =
            mkApp scripts.rustfmt "clipper-rustfmt"
              "Format Rust sources with the pinned nightly toolchain";
          wasm-check =
            mkApp scripts.wasm-check "clipper-wasm-check"
              "Check the Rust app crate for wasm32-unknown-unknown";
          frb-generate =
            mkApp scripts.frb-generate "clipper-frb-generate"
              "Regenerate Flutter Rust Bridge bindings";
          frb-build-web =
            mkApp scripts.frb-build-web "clipper-frb-build-web"
              "Build Flutter Rust Bridge wasm artifacts";
          web-build = mkApp scripts.web-build "clipper-web-build" "Build the Flutter web application";
          web-serve =
            mkApp scripts.web-serve "clipper-web-serve"
              "Serve the Flutter web build with required cross-origin isolation headers";
        }
      );

      formatter = forAllSystems (system: (mkCommandScripts (mkPkgs system)).fmt);
    };
}
