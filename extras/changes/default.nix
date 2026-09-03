{
  changesPackage,
  jq,
  mkProvider,
}:
mkProvider {
  name = "changes";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    jq
    changesPackage
  ];
  commandPackages = [ changesPackage ];
}
