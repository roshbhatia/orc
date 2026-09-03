{
  coreutils,
  jq,
  mkProvider,
  zmx,
}:
mkProvider {
  name = "zmx";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    coreutils
    jq
    zmx
  ];
  commandPackages = [ zmx ];
}
