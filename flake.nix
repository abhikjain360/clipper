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
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
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
                cmake
                dart
                deno
                flutter
                jdk17
                llvmPackages.clang
                llvmPackages.libclang
                ninja
                openssl
                pkg-config
                rust-analyzer
                rustup
                sea-orm-cli
                sqlite
              ])
              ++ darwinInputs;

            env = {
              COCOAPODS_DISABLE_STATS = "1";
              JAVA_HOME = "${pkgs.jdk17.home}";
              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              RUST_BACKTRACE = "1";
            };

            shellHook = ''
              export PATH="$HOME/.cargo/bin:$PATH"

              rust_targets="aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android"
              if command -v rustup >/dev/null 2>&1; then
                if ! rustup toolchain list | grep -Eq '^stable(-| )'; then
                  rustup toolchain install stable \
                    --profile minimal \
                    --component rustfmt,clippy,rust-src \
                    --target $rust_targets \
                    --no-self-update
                else
                  rustup component add --toolchain stable rustfmt clippy rust-src >/dev/null 2>&1 \
                    || echo "warning: failed to install Rust components for stable"
                  rustup target add --toolchain stable $rust_targets >/dev/null 2>&1 \
                    || echo "warning: failed to install Android Rust targets for stable"
                fi
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

      formatter = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        pkgs.writeShellApplication {
          name = "clipper-fmt";
          runtimeInputs = [ pkgs.nixfmt ];
          text = ''
            if [ "$#" -eq 0 ]; then
              set -- flake.nix
            fi
            exec nixfmt "$@"
          '';
        }
      );
    };
}
