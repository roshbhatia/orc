{ jq, mkProvider }:
mkProvider {
  name = "harness";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [ jq ];
}
