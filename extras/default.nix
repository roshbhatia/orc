{
  callPackage,
  changesPackage,
  lib,
  symlinkJoin,
  tracesPackage,
}:
let
  mkProvider = callPackage ./lib/mk-provider.nix { };
  entries = builtins.readDir ./.;
  providerNames = builtins.filter (
    name: entries.${name} == "directory" && builtins.pathExists (./. + "/${name}/default.nix")
  ) (builtins.attrNames entries);
  providers = lib.genAttrs providerNames (
    name:
    let
      path = ./. + "/${name}";
      arguments = builtins.functionArgs (import path);
      overrides = lib.intersectAttrs arguments {
        inherit changesPackage mkProvider tracesPackage;
      };
    in
    callPackage path overrides
  );
in
providers
// {
  all = symlinkJoin {
    name = "orc-providers";
    paths = map (provider: provider.adapter) (lib.attrValues providers);
    passthru.providers = providers;
  };
}
