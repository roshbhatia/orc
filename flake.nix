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
  };

  outputs =
    inputs:
    let
      eachSystem = inputs.nixpkgs.lib.genAttrs (import inputs.systems);
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
          providers = pkgs.callPackage ./extras { };
        in
        {
          default = core;
          inherit core;
          extras = providers.all;
          full = pkgs.symlinkJoin {
            name = "orc-full";
            paths = [
              core
              providers.all
            ];
          };
          provider-changes = providers.changes;
          provider-harness = providers.harness;
          provider-local = providers.local;
          provider-traces = providers.traces;
          provider-wezterm = providers.wezterm;
          provider-zmx = providers.zmx;
        }
      );

      apps = eachSystem (system: {
        default = {
          type = "app";
          program = "${inputs.nixpkgs.lib.getExe inputs.self.packages.${system}.default}";
        };
      });

      checks = eachSystem (system: {
        default = inputs.self.packages.${system}.default;
        providers = inputs.self.packages.${system}.extras;
      });

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
