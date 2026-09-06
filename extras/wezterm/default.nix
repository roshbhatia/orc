{
  coreutils,
  jq,
  mkProvider,
  procps,
  stdenv,
  unixtools,
  wezterm,
}:
mkProvider {
  name = "wezterm";
  manifest = ./provider.yaml;
  script = ./provider.sh;
  runtimeInputs = [
    coreutils
    jq
    (if stdenv.hostPlatform.isDarwin then unixtools.ps else procps)
    wezterm
  ];
  commandPackages = [ wezterm ];
}
