{ jq, mkProvider }:
mkProvider {
  name = "local";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [ jq ];
}
