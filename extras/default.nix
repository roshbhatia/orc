{
  bash,
  coreutils,
  gnused,
  jq,
  lib,
  symlinkJoin,
  writeShellApplication,
}:
let
  hold = writeShellApplication {
    name = "orc-provider-hold";
    runtimeInputs = [ coreutils ];
    text = ''
      set +e
      "$@"
      code=$?
      set -e
      if ((code != 0)); then
        printf '\nCommand exited with %s. Press Enter to close.\n' "$code"
        IFS= read -r _ || true
      fi
      exit "$code"
    '';
  };
  mkProvider =
    name:
    let
      executable = writeShellApplication {
        name = "orc-provider-${name}";
        runtimeInputs = [
          bash
          coreutils
          gnused
          jq
        ];
        text = ''
          export ORC_PROVIDER_KIND=${lib.escapeShellArg name}
          export ORC_PROVIDER_HOLD=${lib.escapeShellArg (lib.getExe hold)}
          ${builtins.readFile ./provider.sh}
        '';
      };
    in
    symlinkJoin {
      name = "orc-provider-${name}";
      paths = [ executable ];
      postBuild = ''
        mkdir -p "$out/share/orc/providers/${name}"
        cp ${./.}/${name}/provider.yaml "$out/share/orc/providers/${name}/provider.yaml"
      '';
    };
  providers = {
    changes = mkProvider "changes";
    harness = mkProvider "harness";
    local = mkProvider "local";
    traces = mkProvider "traces";
    wezterm = mkProvider "wezterm";
    zmx = mkProvider "zmx";
  };
in
providers
// {
  all = symlinkJoin {
    name = "orc-providers";
    paths = (lib.attrValues providers) ++ [ hold ];
  };
}
