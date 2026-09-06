{
  coreutils,
  jq,
  mkProvider,
  tracesPackage,
}:
mkProvider {
  name = "traces";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    coreutils
    jq
    tracesPackage
  ];
  checkScripts = [ ./test.sh ];
  commandPackages = [ tracesPackage ];
}
