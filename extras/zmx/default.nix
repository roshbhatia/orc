{
  coreutils,
  jq,
  mkProvider,
  python3,
  writeShellApplication,
  zmx,
}:
let
  python = python3.withPackages (packages: [ packages.psutil ]);
  processTree = writeShellApplication {
    name = "orc-provider-zmx-process-tree";
    runtimeInputs = [ python ];
    text = ''
      exec ${python}/bin/python ${./process_tree.py} "$@"
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
