{
  description = "Clipper development environment";

  inputs = {
    # Use unstable until the latest stable Nix channel has Dart >= 3.11.1.
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, fenix, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      rustStableDate = "2026-04-16";
      rustStableManifestSha256 = "sha256-gh/xTkxKHL4eiRXzWv8KP7vfjSk61Iq48x47BEDFgfk=";
      rustNightlyDate = "2026-05-24";
      rustNightlyManifestSha256 = "sha256-gARSjceSEFY+8IYGJFhN8O+oqKPN/eyMFW+aqGVu9hk=";
      androidRustTargets = [
        "aarch64-linux-android"
        "armv7-linux-androideabi"
        "i686-linux-android"
        "x86_64-linux-android"
      ];
      wasmRustTarget = "wasm32-unknown-unknown";
      # Interpolate the directory so every script shares one /nix/store path
      # and the BASH_SOURCE-derived SCRIPT_DIR can resolve sibling helpers.
      scriptsDir = ./scripts;
      mkPkgs = system: import nixpkgs { inherit system; };
      mkRustToolchains =
        system:
        let
          fenixPkgs = fenix.packages.${system};
          stableArgs = {
            channel = "stable";
            date = rustStableDate;
            sha256 = rustStableManifestSha256;
          };
          stableChannel = fenixPkgs.toolchainOf stableArgs;
          nightlyArgs = {
            channel = "nightly";
            date = rustNightlyDate;
            sha256 = rustNightlyManifestSha256;
          };
          nightlyChannel = fenixPkgs.toolchainOf nightlyArgs;
        in
        {
          stable = fenixPkgs.combine (
            [
              stableChannel.cargo
              stableChannel.rustc
              stableChannel.rustfmt
              stableChannel.clippy
              stableChannel.rust-src
              stableChannel.rust-analyzer
            ]
            ++ map (t: (fenixPkgs.targets.${t}.toolchainOf stableArgs).rust-std) (
              androidRustTargets ++ [ wasmRustTarget ]
            )
          );
          nightly = fenixPkgs.combine [
            nightlyChannel.cargo
            nightlyChannel.rustc
            nightlyChannel.rustfmt
            nightlyChannel.rust-src
            (fenixPkgs.targets.${wasmRustTarget}.toolchainOf nightlyArgs).rust-std
          ];
        };
      mkCommandSpecs =
        system:
        let
          pkgs = mkPkgs system;
          toolchains = mkRustToolchains system;
          baseRuntimeInputs = with pkgs; [
            bash
            coreutils
            git
          ];
          fmtRuntimeInputs =
            baseRuntimeInputs
            ++ (with pkgs; [
              dart
              deno
              nixfmt
            ])
            ++ [ toolchains.nightly ];
          rustRuntimeInputs = baseRuntimeInputs ++ [ toolchains.nightly ];
          frbGenerateRuntimeInputs =
            baseRuntimeInputs
            ++ (with pkgs; [
              dart
              flutter
              flutter_rust_bridge_codegen
            ])
            ++ [ toolchains.stable ];
          frbWebRuntimeInputs =
            frbGenerateRuntimeInputs
            ++ (with pkgs; [
              wasm-pack
            ])
            ++ [ toolchains.nightly ];
          webServeRuntimeInputs = baseRuntimeInputs ++ [ pkgs.deno ];
          macosBuildRuntimeInputs =
            baseRuntimeInputs
            ++ [ pkgs.deno ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.cocoapods
              toolchains.stable
            ];
          nightlyEnv = ''
            export CLIPPER_RUST_NIGHTLY_BIN="${toolchains.nightly}/bin"
          '';
          stableEnv = ''
            export CLIPPER_STABLE_BIN="${toolchains.stable}/bin"
          '';
          wasmEnv = ''
            export CLIPPER_WASM_TARGET="${wasmRustTarget}"
          '';
          flutterEnv = ''
            export FLUTTER_ROOT="${pkgs.flutter}"
          '';
          cargokitStableEnv = stableEnv + ''
            export CARGOKIT_CARGO="${toolchains.stable}/bin/cargo"
            export CARGOKIT_RUSTC="${toolchains.stable}/bin/rustc"
          '';
        in
        {
          fmt = {
            program = "clipper-fmt";
            script = "fmt.sh";
            description = "Format all Clipper sources";
            runtimeInputs = fmtRuntimeInputs;
            env = nightlyEnv;
          };

          rustfmt = {
            program = "clipper-rustfmt";
            script = "rustfmt.sh";
            description = "Format Rust sources with the pinned nightly toolchain";
            runtimeInputs = rustRuntimeInputs;
            env = nightlyEnv;
          };

          wasm-check = {
            program = "clipper-wasm-check";
            script = "wasm-check.sh";
            description = "Check the Rust app crate for wasm32-unknown-unknown";
            runtimeInputs = rustRuntimeInputs;
            env = nightlyEnv + wasmEnv;
          };

          frb-generate = {
            program = "clipper-frb-generate";
            script = "frb-generate.sh";
            description = "Regenerate Flutter Rust Bridge bindings";
            runtimeInputs = frbGenerateRuntimeInputs;
            env = flutterEnv;
          };

          server-entities = {
            program = "clipper-server-entities";
            script = "server-entities.sh";
            description = "Regenerate SeaORM server entities from the current schema";
            runtimeInputs =
              baseRuntimeInputs
              ++ (with pkgs; [
                sea-orm-cli
                sqlite
              ])
              ++ [ toolchains.stable ];
            env = stableEnv;
          };

          frb-build-web = {
            program = "clipper-frb-build-web";
            script = "frb-build-web.sh";
            description = "Build Flutter Rust Bridge wasm artifacts";
            runtimeInputs = frbWebRuntimeInputs;
            env = nightlyEnv + flutterEnv;
          };

          web-build = {
            program = "clipper-web-build";
            script = "web-build.sh";
            description = "Build the Flutter web application";
            runtimeInputs = frbWebRuntimeInputs;
            env = nightlyEnv + wasmEnv + flutterEnv;
          };

          web-serve = {
            program = "clipper-web-serve";
            denoScript = "web-serve.ts";
            denoPermissions = [
              "--allow-env=CLIPPER_WEB_ROOT,CLIPPER_REPO_ROOT"
              "--allow-read"
              "--allow-net=127.0.0.1"
              "--allow-run=git"
            ];
            description = "Serve the Flutter web build with required cross-origin isolation headers";
            runtimeInputs = webServeRuntimeInputs;
          };
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.isDarwin {
          macos-build = {
            program = "clipper-macos-build";
            denoScript = "macos-build.ts";
            denoPermissions = [
              "--allow-env"
              "--allow-read"
              "--allow-run"
            ];
            description = "Build the macOS Flutter app with host Flutter/Xcode and Nix Rust";
            runtimeInputs = macosBuildRuntimeInputs;
            env = cargokitStableEnv;
          };
        };
      mkCommandScripts =
        system:
        let
          pkgs = mkPkgs system;
          specs = mkCommandSpecs system;
          mkCommandText =
            cfg:
            let
              denoPermissions = pkgs.lib.escapeShellArgs (cfg.denoPermissions or [ ]);
            in
            if cfg ? denoScript then
              ''
                ${cfg.env or ""}
                deno run ${denoPermissions} ${scriptsDir}/${cfg.denoScript} "$@"
              ''
            else
              ''
                ${cfg.env or ""}
                bash ${scriptsDir}/${cfg.script} "$@"
              '';
        in
        pkgs.lib.mapAttrs (
          _name: cfg:
          pkgs.writeShellApplication {
            name = cfg.program;
            runtimeInputs = cfg.runtimeInputs;
            text = mkCommandText cfg;
          }
        ) specs;
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
          toolchains = mkRustToolchains system;
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
                sea-orm-cli
                sqlite
                wasm-pack
              ])
              ++ [
                toolchains.stable
              ]
              ++ darwinInputs;

            env = {
              CLIPPER_STABLE_BIN = "${toolchains.stable}/bin";
              CLIPPER_WASM_TARGET = "${wasmRustTarget}";
              CARGOKIT_CARGO = "${toolchains.stable}/bin/cargo";
              CARGOKIT_RUSTC = "${toolchains.stable}/bin/rustc";
              CLIPPER_RUST_NIGHTLY_BIN = "${toolchains.nightly}/bin";
              COCOAPODS_DISABLE_STATS = "1";
              FLUTTER_ROOT = "${pkgs.flutter}";
              JAVA_HOME = "${pkgs.jdk17.home}";
              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              RUST_BACKTRACE = "1";
              RUST_SRC_PATH = "${toolchains.stable}/lib/rustlib/src/rust/library";
            };

            shellHook = ''
              source ${scriptsDir}/dev-shell-env.sh
            '';
          };
        }
      );

      packages = forAllSystems (
        system:
        let
          scripts = mkCommandScripts system;
        in
        scripts // { default = scripts.fmt; }
      );

      apps = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          scripts = mkCommandScripts system;
          specs = mkCommandSpecs system;
        in
        pkgs.lib.mapAttrs (name: drv: mkApp drv specs.${name}.program specs.${name}.description) scripts
        // {
          default = mkApp scripts.fmt specs.fmt.program specs.fmt.description;
        }
      );

      formatter = forAllSystems (system: (mkCommandScripts system).fmt);
    };
}
