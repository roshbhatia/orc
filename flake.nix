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
        in
        {
          default = core;
          inherit core;
        }
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
        in
        {
          default = inputs.self.packages.${system}.default;
          workflows = pkgs.runCommand "orc-workflows" { nativeBuildInputs = [ pkgs.actionlint ]; } ''
            actionlint ${./.github/workflows/ci.yml} ${./.github/workflows/release.yml}
            touch "$out"
          '';
        }
      );

      devShells = eachSystem (system: {
        default = pkgsFor.${system}.mkShellNoCC {
          packages = with pkgsFor.${system}; [
            actionlint
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
            ttyd
            vhs
          ];
        };
      });
    };
}
