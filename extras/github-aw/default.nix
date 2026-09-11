{
  gh,
  jq,
  mkProvider,
}:
mkProvider {
  name = "github-aw";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    gh
    jq
  ];
  checkScripts = [ ./test.sh ];
}
