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
                cargo
                clippy
                dart
                deno
                flutter
                llvmPackages.clang
                llvmPackages.libclang
                openssl
                pkg-config
                rust-analyzer
                rustc
                rustfmt
                sea-orm-cli
                sqlite
              ])
              ++ darwinInputs;

            env = {
              COCOAPODS_DISABLE_STATS = "1";
              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              RUST_BACKTRACE = "1";
              RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            };

            shellHook = ''
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
              fi

              echo "clipper dev shell"
              echo "  rust: $(rustc --version)"
              echo "  flutter: $(flutter --version | sed -n '1p')"
              echo "  sea-orm-cli: $(sea-orm-cli --version)"
              echo "platform builds still use host Xcode/CocoaPods/Android SDK where Flutter requires them"
            '';
          };
        }
      );

      formatter = forAllSystems (system: (import nixpkgs { inherit system; }).nixfmt);
    };
}
