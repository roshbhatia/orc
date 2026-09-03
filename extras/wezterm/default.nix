{
  coreutils,
  jq,
  mkProvider,
  wezterm,
}:
mkProvider {
  name = "wezterm";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    coreutils
    jq
    wezterm
  ];
  commandPackages = [ wezterm ];
}
