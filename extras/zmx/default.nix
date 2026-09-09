{
  callPackage,
  coreutils,
  jq,
  mkProvider,
  writeShellApplication,
  zmx,
}:
let
  python = callPackage ./python.nix { };
  processTree = writeShellApplication {
    name = "orc-provider-zmx-process-tree";
    runtimeInputs = [ python ];
    text = ''
      exec ${python}/bin/orc-zmx-python ${./process_tree.py} "$@"
    '';
  };
in
mkProvider {
  name = "zmx";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    coreutils
    jq
    processTree
    zmx
  ];
  commandPackages = [ zmx ];
}
