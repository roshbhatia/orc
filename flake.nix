{
  description = "Local control plane for agent harnesses";

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    systems.url = "github:nix-systems/default";
    changes = {
      url = "github:roshbhatia/changes/main";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.systems.follows = "systems";
    };
    traces = {
      url = "github:roshbhatia/traces/main";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.systems.follows = "systems";
    };
  };

  outputs =
    inputs:
    let
      lib = inputs.nixpkgs.lib;
      supportedSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      eachSystem = lib.genAttrs supportedSystems;
      pkgsFor = eachSystem (
        system:
        import inputs.nixpkgs {
          inherit system;
        }
      );
    in
    {
      formatter = eachSystem (
        system:
        let
          pkgs = pkgsFor.${system};
        in
        pkgs.writeShellApplication {
          name = "orc-format";
          runtimeInputs = [
            pkgs.fd
            pkgs.nixfmt
          ];
          text = ''
            if [ "$#" -gt 0 ] && [ "''${1#-}" = "$1" ]; then
              exec nixfmt "$@"
            fi
            exec fd --extension nix --type file --exec-batch nixfmt "$@"
          '';
        }
      );

      packages = eachSystem (
        system:
        let
          pkgs = pkgsFor.${system};
          core = pkgs.callPackage ./package.nix { };
          providerRegistry = pkgs.callPackage ./extras {
            changesPackage = inputs.changes.packages.${system}.default;
            tracesPackage = inputs.traces.packages.${system}.default;
          };
          providers = providerRegistry.all.providers;
        in
        {
          default = core;
          inherit core;
          extras = providerRegistry.all;
          full = pkgs.symlinkJoin {
            name = "orc-full";
            paths = [
              core
              providerRegistry.all
            ];
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postBuild = ''
              rm "$out/bin/orc"
              makeWrapper "${core}/bin/orc" "$out/bin/orc" \
                --prefix PATH : "${providerRegistry.all}/bin" \
                --prefix XDG_DATA_DIRS : "${providerRegistry.all}/share"
            '';
          };
        }
        // lib.mapAttrs' (name: provider: lib.nameValuePair "provider-${name}" provider) providers
      );

      apps = eachSystem (system: {
        default = {
          type = "app";
          program = "${inputs.nixpkgs.lib.getExe inputs.self.packages.${system}.default}";
        };
      });

      checks = eachSystem (
        system:
        let
          pkgs = pkgsFor.${system};
          packages = inputs.self.packages.${system};
          providerPackages = packages.extras.providers;
          providerNames = builtins.attrNames providerPackages;
          productPackages = {
            changes = inputs.changes.packages.${system}.default;
            traces = inputs.traces.packages.${system}.default;
            wezterm = pkgs.wezterm;
            zmx = pkgs.zmx;
          };
          registry = pkgs.writeText "orc-provider-validation-agents.json" (
            builtins.toJSON {
              agents = [
                {
                  name = "orc-provider-validation";
                  command = "true";
                  launch.resumeArgs = [ "resume" ];
                }
              ];
            }
          );
          validationChecks = lib.mapAttrs' (
            name: provider:
            lib.nameValuePair "provider-${name}-validation" (
              pkgs.runCommand "orc-provider-${name}-validation"
                {
                  nativeBuildInputs = [
                    packages.core
                    pkgs.jq
                    provider
                  ];
                }
                ''
                  export HOME="$TMPDIR/home"
                  export XDG_CONFIG_HOME="$TMPDIR/config"
                  export ORC_PROVIDERS_DIRECTORY="$XDG_CONFIG_HOME/orc/providers"
                  export ORC_AGENT_REGISTRY=${registry}
                  mkdir -p "$ORC_PROVIDERS_DIRECTORY"
                  cp -R ${provider}/share/orc/providers/${name} "$ORC_PROVIDERS_DIRECTORY/${name}"
                  orc provider validate ${name} --scope "$TMPDIR" --json > result.json
                  jq -e '
                    length == 1
                    and .[0].provider.name == "${name}"
                    and .[0].status == "ok"
                    and all(.[0].checks[]; .status == "ok")
                  ' result.json > /dev/null
                  touch "$out"
                ''
            )
          ) providerPackages;
          closureChecks = lib.mapAttrs' (
            name: provider:
            let
              closure = pkgs.closureInfo { rootPaths = [ provider ]; };
              forbiddenProducts = lib.attrValues (builtins.removeAttrs productPackages [ name ]);
              otherProviderNames = builtins.filter (candidate: candidate != name) providerNames;
              requiredProducts = provider.commandPackages;
              requireProduct = package: ''
                grep -Fx ${lib.escapeShellArg (toString package)} ${closure}/store-paths > /dev/null
              '';
              rejectProduct = package: ''
                if grep -Fx ${lib.escapeShellArg (toString package)} ${closure}/store-paths > /dev/null; then
                  printf 'provider %s contains unrelated product %s\n' \
                    ${lib.escapeShellArg name} ${lib.escapeShellArg (toString package)} >&2
                  exit 1
                fi
              '';
            in
            lib.nameValuePair "provider-${name}-closure" (
              pkgs.runCommand "orc-provider-${name}-closure" { } ''
                test -x ${provider}/bin/orc-provider-${name}
                test -f ${provider}/share/orc/providers/${name}/provider.yaml
                ${lib.concatMapStringsSep "\n" (other: ''
                  test ! -e ${provider}/bin/orc-provider-${other}
                  test ! -e ${provider}/share/orc/providers/${other}
                '') otherProviderNames}
                ${lib.concatMapStringsSep "\n" requireProduct requiredProducts}
                ${lib.concatMapStringsSep "\n" rejectProduct forbiddenProducts}
                touch "$out"
              ''
            )
          ) providerPackages;
          providerNeutralCore =
            pkgs.runCommand "orc-provider-neutral-core"
              {
                nativeBuildInputs = [ pkgs.ripgrep ];
              }
              ''
                if [ -e ${packages.core.src}/extras ]; then
                  printf 'core source contains the extras tree\n' >&2
                  exit 1
                fi
                if rg -n -i '\b(zmx|wezterm|traces)\b|orc-provider-(changes|traces|wezterm|zmx)|Command::new\("changes"\)|command:[[:space:]]+changes\b' \
                  ${packages.core.src} > matches; then
                  printf 'core source contains provider product names:\n' >&2
                  cat matches >&2
                  exit 1
                fi
                touch "$out"
              '';
          coreClosure = pkgs.closureInfo { rootPaths = [ packages.core ]; };
          providerNeutralClosure = pkgs.runCommand "orc-provider-neutral-closure" { } ''
            ${lib.concatMapStringsSep "\n" (package: ''
              if grep -Fx ${lib.escapeShellArg (toString package)} ${coreClosure}/store-paths > /dev/null; then
                printf 'core closure contains provider product %s\n' \
                  ${lib.escapeShellArg (toString package)} >&2
                exit 1
              fi
            '') (lib.attrValues productPackages)}
            touch "$out"
          '';
          installedProviderDiscovery =
            pkgs.runCommand "orc-installed-provider-discovery"
              {
                nativeBuildInputs = [
                  packages.core
                  pkgs.jq
                ];
              }
              ''
                export HOME="$TMPDIR/home"
                export XDG_CONFIG_HOME="$TMPDIR/config"
                export XDG_DATA_HOME="$TMPDIR/data"
                export XDG_DATA_DIRS="${providerPackages.harness}/share"
                orc provider list --json > providers.json
                jq -e '
                  length == 1
                  and .[0].name == "harness"
                ' providers.json > /dev/null
                touch "$out"
              '';
          weztermEnvironment =
            let
              fakeWezterm = pkgs.writeShellScriptBin "wezterm" ''
                exit 0
              '';
            in
            pkgs.runCommand "orc-wezterm-composed-environment"
              {
                nativeBuildInputs = [
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.jq
                  fakeWezterm
                ];
              }
              ''
                export HOME="$TMPDIR/home"
                export ORC_PROVIDER_LIB=${./extras/lib/provider.sh}
                jq -n \
                  --arg scope "$TMPDIR" \
                  '{
                    version: "orc.provider/v1",
                    capability: "terminal.open",
                    scope: $scope,
                    direction: "right",
                    plan: {
                      version: "orc.provider/v1",
                      command: ["printenv", "ORC_COMPOSED_TEST"],
                      cwd: $scope,
                      environment: {ORC_COMPOSED_TEST: "preserved"},
                      successCodes: [0]
                    }
                  }' \
                  | bash ${./extras/wezterm/provider.sh} > plan.json
                jq -e '
                  . as $plan
                  | ($plan.command | index("--")) as $separator
                  | $plan.environment.ORC_COMPOSED_TEST == "preserved"
                  and ($plan.command[$separator + 1] | endswith("/env"))
                ' plan.json > /dev/null
                jq -e '
                  .command as $command
                  | ($command | index("--")) as $separator
                  | $command[$separator + 2] == "ORC_COMPOSED_TEST=preserved"
                  and $command[$separator + 4] == "hold"
                  and $command[$separator + 5:] == ["printenv", "ORC_COMPOSED_TEST"]
                ' plan.json > /dev/null
                touch "$out"
              '';
        in
        {
          default = packages.default;
          full = packages.full;
          providers = packages.extras;
          provider-neutral-closure = providerNeutralClosure;
          provider-neutral-core = providerNeutralCore;
          installed-provider-discovery = installedProviderDiscovery;
          wezterm-composed-environment = weztermEnvironment;
        }
        // validationChecks
        // closureChecks
      );

      devShells = eachSystem (system: {
        default = pkgsFor.${system}.mkShellNoCC {
          packages = with pkgsFor.${system}; [
            cargo
            clippy
            ffmpeg
            fish
            git
            jq
            ripgrep
            rustc
            rustfmt
            shellcheck
            shfmt
            vhs
          ];
        };
      });
    };
}
