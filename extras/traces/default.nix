{
  jq,
  mkProvider,
  tracesPackage,
}:
mkProvider {
  name = "traces";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    jq
    tracesPackage
  ];
  commandPackages = [ tracesPackage ];
}
