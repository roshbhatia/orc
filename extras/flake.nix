{
  description = "Optional provider integrations for Orc";

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    systems.url = "github:nix-systems/default";
    orc = {
      url = "path:..";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.systems.follows = "systems";
    };
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
      formatter = eachSystem (system: inputs.orc.formatter.${system});

      packages = eachSystem (
        system:
        let
          pkgs = pkgsFor.${system};
          core = inputs.orc.packages.${system}.core;
          providerRegistry = pkgs.callPackage ./. {
            changesPackage = inputs.changes.packages.${system}.default;
            tracesPackage = inputs.traces.packages.${system}.default;
          };
          providers = providerRegistry.all.providers;
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
                --prefix XDG_DATA_DIRS : "${providerRegistry.all}/share" \
                --set ORC_RUNTIME_GENERATION "${providerRegistry.all}"
            '';
            meta = core.meta // {
              description = "Orc with the bundled provider integrations";
              mainProgram = "orc";
            };
          };
        in
        {
          default = providerRegistry.all;
          all = providerRegistry.all;
          inherit full;
        }
        // lib.mapAttrs' (name: provider: lib.nameValuePair "provider-${name}" provider) providers
      );

      checks = eachSystem (
        system:
        let
          pkgs = pkgsFor.${system};
          packages = inputs.self.packages.${system};
          core = inputs.orc.packages.${system}.core;
          providerPackages = packages.all.providers;
          providerNames = builtins.attrNames providerPackages;
          productPackages = {
            changes = inputs.changes.packages.${system}.default;
            traces = inputs.traces.packages.${system}.default;
            wezterm = pkgs.wezterm;
            zmx = pkgs.zmx;
          };
          validationHarness = pkgs.writeShellScriptBin "orc-provider-validation" ''
            exit 0
          '';
          registry = pkgs.writeText "orc-provider-validation-agents.json" (
            builtins.toJSON {
              agents = [
                {
                  name = "orc-provider-validation";
                  command = lib.getExe validationHarness;
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
                    core
                    pkgs.jq
                  ];
                }
                ''
                  validation_path=${
                    lib.makeBinPath [
                      core
                      provider.adapter
                      pkgs.coreutils
                      pkgs.jq
                    ]
                  }
                  env -i \
                    HOME="$TMPDIR/home" \
                    ORC_AGENT_REGISTRY=${registry} \
                    ORC_PROVIDERS_DIRECTORY="$TMPDIR/config/orc/providers" \
                    PATH="$validation_path" \
                    TMPDIR="$TMPDIR" \
                    XDG_CONFIG_HOME="$TMPDIR/config" \
                    XDG_DATA_HOME="$TMPDIR/data" \
                    ${pkgs.runtimeShell} -c '
                      mkdir -p "$ORC_PROVIDERS_DIRECTORY"
                      cp -R ${provider.adapter}/share/orc/providers/${name} "$ORC_PROVIDERS_DIRECTORY/${name}"
                      orc provider validate ${name} --scope "$TMPDIR" --json > result.json
                    '
                  jq -e '
                    length == 1
                    and .[0].provider.name == "${name}"
                    and .[0].status == "ok"
                    and all(.[0].checks[]; .status == "ok")
                  ' result.json > /dev/null || {
                    cat result.json >&2
                    exit 1
                  }
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
          coreClosure = pkgs.closureInfo { rootPaths = [ core ]; };
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
          providerNeutralSource =
            pkgs.runCommand "orc-provider-neutral-source"
              {
                nativeBuildInputs = [ pkgs.ripgrep ];
              }
              ''
                set +e
                rg --line-number --ignore-case '\b(zmx|wezterm|traces)\b' ${inputs.orc}/src
                result=$?
                set -e
                case $result in
                  0)
                    printf 'core source contains a concrete provider product\n' >&2
                    exit 1
                    ;;
                  1) ;;
                  *) exit "$result" ;;
                esac
                touch "$out"
              '';
          installedProviderDiscovery =
            pkgs.runCommand "orc-installed-provider-discovery"
              {
                nativeBuildInputs = [
                  core
                  pkgs.jq
                ];
              }
              ''
                export HOME="$TMPDIR/home"
                export XDG_CONFIG_HOME="$TMPDIR/config"
                export XDG_DATA_HOME="$TMPDIR/data"
                export XDG_DATA_DIRS="${providerPackages.harness}/share"
                orc provider list --json > providers.json
                jq -e 'length == 1 and .[0].name == "harness"' providers.json > /dev/null
                touch "$out"
              '';
          providerAggregateBoundary = pkgs.runCommand "orc-provider-aggregate-boundary" { } ''
            ${lib.concatMapStringsSep "\n" (name: ''
              test -x ${packages.all}/bin/orc-provider-${name}
              test ! -e ${packages.all}/bin/${name}
              test -f ${packages.all}/share/orc/providers/${name}/provider.yaml
            '') providerNames}
            touch "$out"
          '';
          fullMainProgram =
            assert packages.full.meta.mainProgram == "orc";
            pkgs.runCommand "orc-full-main-program" { } ''
              test ${lib.getExe packages.full} = ${packages.full}/bin/orc
              test -x ${lib.getExe packages.full}
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
                export ORC_PROVIDER_LIB=${./lib/provider.sh}
                export ORC_PROVIDER_WEZTERM_SCRIPT=${./wezterm/provider.sh}
                bash ${./wezterm/test.sh}
                touch "$out"
              '';
          zmxLifecycle =
            pkgs.runCommand "orc-zmx-lifecycle"
              {
                nativeBuildInputs = [
                  pkgs.bash
                  pkgs.coreutils
                  pkgs.jq
                ];
              }
              ''
                export HOME="$TMPDIR/home"
                export ORC_PROVIDER_LIB=${./lib/provider.sh}
                export ORC_PROVIDER_ZMX_SCRIPT=${./zmx/provider.sh}
                export ORC_TEST_BASH=${pkgs.lib.getExe pkgs.bash}
                bash ${./zmx/test.sh}
                touch "$out"
              '';
        in
        {
          default = packages.default;
          full = packages.full;
          provider-neutral-closure = providerNeutralClosure;
          provider-neutral-source = providerNeutralSource;
          provider-aggregate-boundary = providerAggregateBoundary;
          full-main-program = fullMainProgram;
          installed-provider-discovery = installedProviderDiscovery;
          wezterm-composed-environment = weztermEnvironment;
          zmx-lifecycle = zmxLifecycle;
        }
        // validationChecks
        // closureChecks
      );
    };
}
