{
  description = "Clipper development environment";

  inputs = {
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
      rustStableDate = "2026-05-28";
      rustStableManifestSha256 = "sha256-mvUGEOHYJpn3ikC5hckneuGixaC+yGrkMM/liDIDgoU=";
      rustNightlyDate = "2026-06-04";
      rustNightlyManifestSha256 = "sha256-gQBIgkaAydtk9H+rmJBeyHNfZr/m1GybGmyApwvGF8E=";
      wasmRustTarget = "wasm32-unknown-unknown";
      androidRustTargets = [
        "aarch64-linux-android"
        "armv7-linux-androideabi"
        "i686-linux-android"
        "x86_64-linux-android"
      ];
      stableRustTargets = [ wasmRustTarget ] ++ androidRustTargets;
      # Interpolate the directory so every script shares one /nix/store path
      # and can resolve sibling helpers.
      scriptsDir = ./scripts;
      mkPkgs = system: import nixpkgs { inherit system; };
      raiseOpenFileLimit = ''
        case "$(ulimit -n)" in
          unlimited) ;;
          *[!0-9]*) ;;
          *)
            if [ "$(ulimit -n)" -lt 4096 ]; then
              ulimit -n 4096 2>/dev/null || true
            fi
            ;;
        esac
      '';
      mkPnpm =
        pkgs:
        pkgs.writeShellApplication {
          name = "pnpm";
          runtimeInputs = [ pkgs.pnpm ];
          text = ''
            ${raiseOpenFileLimit}
            exec ${pkgs.pnpm}/bin/pnpm "$@"
          '';
        };
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
            ++ map (t: (fenixPkgs.targets.${t}.toolchainOf stableArgs).rust-std) stableRustTargets
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
          nodejsLts = pkgs.nodejs_24;
          pnpm = mkPnpm pkgs;
          baseRuntimeInputs = with pkgs; [
            bash
            coreutils
            git
          ];
          denoRuntimeInputs = baseRuntimeInputs ++ [ pkgs.deno ];
          jsRuntimeInputs = [
            nodejsLts
            pnpm
          ];
          fmtRuntimeInputs =
            denoRuntimeInputs
            ++ (with pkgs; [
              nixfmt
            ])
            ++ jsRuntimeInputs
            ++ [ toolchains.nightly ];
          rustRuntimeInputs = denoRuntimeInputs ++ [ toolchains.nightly ];
          webRuntimeInputs =
            denoRuntimeInputs
            ++ (with pkgs; [
              wasm-pack
            ])
            ++ jsRuntimeInputs
            ++ [ toolchains.stable ];
          mobileRuntimeInputs =
            denoRuntimeInputs
            ++ jsRuntimeInputs
            ++ (with pkgs; [
              cargo-ndk
            ])
            ++ [ toolchains.stable ];
          tauriRuntimeInputs =
            webRuntimeInputs
            ++ (with pkgs; [
              openssl
              pkg-config
            ])
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux (
              with pkgs;
              [
                atk
                cairo
                gdk-pixbuf
                glib
                gtk3
                libayatana-appindicator
                libsoup_3
                pango
                webkitgtk_4_1
              ]
            )
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.apple-sdk_15
              pkgs.libiconv
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
          denoTaskPermissions = [
            "--allow-env"
            "--allow-read"
            "--allow-run"
          ];
          mobileTaskPermissions = [
            "--allow-env"
            "--allow-read"
            "--allow-write"
            "--allow-run"
          ];
          mkTaskSpec = task: {
            denoScript = "tasks.ts";
            denoArgs = [ task ];
            denoPermissions = denoTaskPermissions;
          };
        in
        {
          fmt = {
            program = "clipper-fmt";
            description = "Format all Clipper sources";
            runtimeInputs = fmtRuntimeInputs;
            env = nightlyEnv;
          }
          // mkTaskSpec "fmt";

          rustfmt = {
            program = "clipper-rustfmt";
            description = "Format Rust sources with the pinned nightly toolchain";
            runtimeInputs = rustRuntimeInputs;
            env = nightlyEnv;
          }
          // mkTaskSpec "rustfmt";

          audit = {
            program = "clipper-audit";
            description = "Scan dependency manifests with OSV-Scanner";
            runtimeInputs =
              denoRuntimeInputs
              ++ (with pkgs; [
                osv-scanner
              ])
              ++ [ toolchains.stable ];
            env = stableEnv;
          }
          // mkTaskSpec "audit";

          js-check = {
            program = "clipper-js-check";
            description = "Lint and type check all TypeScript and JavaScript sources";
            runtimeInputs = denoRuntimeInputs ++ jsRuntimeInputs;
          }
          // mkTaskSpec "js-check";

          udeps = {
            program = "clipper-udeps";
            description = "Detect unused Rust dependencies with cargo-udeps";
            runtimeInputs =
              denoRuntimeInputs
              ++ (with pkgs; [
                cargo-udeps
              ])
              ++ [ toolchains.nightly ];
            env = nightlyEnv;
          }
          // mkTaskSpec "udeps";

          wasm-check = {
            program = "clipper-wasm-check";
            description = "Check the standalone web wasm crate for wasm32-unknown-unknown";
            runtimeInputs = denoRuntimeInputs ++ [ toolchains.stable ];
            env = stableEnv + wasmEnv;
          }
          // mkTaskSpec "wasm-check";

          server-entities = {
            program = "clipper-server-entities";
            denoScript = "server-entities.ts";
            denoPermissions = [
              "--allow-env"
              "--allow-read"
              "--allow-write"
              "--allow-run"
            ];
            description = "Regenerate SeaORM server entities from the current schema";
            runtimeInputs =
              denoRuntimeInputs
              ++ (with pkgs; [
                sea-orm-cli
                sqlite
              ])
              ++ [ toolchains.stable ];
            env = stableEnv;
          };

          web-build = {
            program = "clipper-web-build";
            description = "Build the standalone web application";
            runtimeInputs = webRuntimeInputs;
            env = stableEnv + wasmEnv;
          }
          // mkTaskSpec "web-build";

          web-serve = {
            program = "clipper-web-serve";
            description = "Serve the standalone web application with Vite";
            runtimeInputs = webRuntimeInputs;
            env = stableEnv + wasmEnv;
          }
          // mkTaskSpec "web-serve";

          web-check = {
            program = "clipper-web-check";
            description = "Lint and type check the standalone web application";
            runtimeInputs = webRuntimeInputs;
            env = stableEnv + wasmEnv;
          }
          // mkTaskSpec "web-check";

          tauri-dev = {
            program = "clipper-tauri-dev";
            description = "Run the Tauri desktop shell";
            runtimeInputs = tauriRuntimeInputs;
            env = stableEnv + wasmEnv;
          }
          // mkTaskSpec "tauri-dev";

          tauri-build = {
            program = "clipper-tauri-build";
            description = "Build the Tauri desktop shell";
            runtimeInputs = tauriRuntimeInputs;
            env = stableEnv + wasmEnv;
          }
          // mkTaskSpec "tauri-build";

          mobile-check = {
            program = "clipper-mobile-check";
            description = "Type check and lint the React Native mobile packages";
            runtimeInputs = mobileRuntimeInputs;
            env = stableEnv;
          }
          // mkTaskSpec "mobile-check";

          mobile-start = {
            program = "clipper-mobile-start";
            description = "Start the Expo React Native development server";
            runtimeInputs = mobileRuntimeInputs;
            env = stableEnv;
          }
          // mkTaskSpec "mobile-start";

          mobile-android = {
            program = "clipper-mobile-android";
            description = "Generate the UniFFI bridge and run the Android React Native app";
            runtimeInputs = mobileRuntimeInputs;
            env = stableEnv;
            denoPermissions = mobileTaskPermissions;
          }
          // mkTaskSpec "mobile-android";

          mobile-uniffi-android = {
            program = "clipper-mobile-uniffi-android";
            description = "Generate the Android React Native UniFFI bridge";
            runtimeInputs = mobileRuntimeInputs;
            env = stableEnv;
            denoPermissions = mobileTaskPermissions;
          }
          // mkTaskSpec "mobile-uniffi-android";
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
              denoScript = pkgs.lib.escapeShellArg "${scriptsDir}/${cfg.denoScript}";
              denoArgs = pkgs.lib.escapeShellArgs (cfg.denoArgs or [ ]);
            in
            if cfg ? text then
              ''
                ${raiseOpenFileLimit}
                ${cfg.env or ""}
                ${cfg.text}
              ''
            else if cfg ? denoScript then
              ''
                ${raiseOpenFileLimit}
                ${cfg.env or ""}
                exec deno run ${denoPermissions} ${denoScript} ${denoArgs} "$@"
              ''
            else
              ''
                ${raiseOpenFileLimit}
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
          nodejsLts = pkgs.nodejs_24;
          pnpm = mkPnpm pkgs;
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
                cargo-ndk
                cargo-udeps
                cmake
                deno
                llvmPackages.clang
                llvmPackages.libclang
                ninja
                nixfmt
                openssl
                osv-scanner
                pkg-config
                sea-orm-cli
                sqlite
                wasm-pack
              ])
              ++ [
                nodejsLts
                pnpm
              ]
              ++ lib.optionals pkgs.stdenv.isLinux (
                with pkgs;
                [
                  atk
                  cairo
                  gdk-pixbuf
                  glib
                  gtk3
                  libayatana-appindicator
                  libsoup_3
                  pango
                  webkitgtk_4_1
                ]
              )
              ++ [
                toolchains.stable
              ]
              ++ darwinInputs;

            env = {
              CLIPPER_NODE_BIN = "${nodejsLts}/bin";
              CLIPPER_PNPM_BIN = "${pnpm}/bin";
              CLIPPER_STABLE_BIN = "${toolchains.stable}/bin";
              CLIPPER_WASM_TARGET = "${wasmRustTarget}";
              CLIPPER_RUST_NIGHTLY_BIN = "${toolchains.nightly}/bin";
              LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
              RUST_BACKTRACE = "1";
              RUST_SRC_PATH = "${toolchains.stable}/lib/rustlib/src/rust/library";
            };

            shellHook = ''
              # Set CLIPPER_DEV_SHELL_BANNER=1 before entering the shell to print tool versions.
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
