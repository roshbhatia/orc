{
  bash,
  lib,
  makeWrapper,
  shellcheck,
  stdenvNoCC,
  symlinkJoin,
}:
{
  checkScripts ? [ ],
  commandPackages ? [ ],
  manifest,
  name,
  runtimeInputs,
  script,
}:
let
  providerRuntimeInputs = [ bash ] ++ runtimeInputs;
  adapter = stdenvNoCC.mkDerivation {
    pname = "orc-provider-${name}-adapter";
    version = "0.11.0";
    dontUnpack = true;
    strictDeps = true;

    nativeBuildInputs = [
      makeWrapper
      shellcheck
    ];

    doCheck = true;
    checkPhase = ''
      shellcheck -x -P ${../.} ${script}
      check_provider="$TMPDIR/orc-provider-${name}"
      cp ${script} "$check_provider"
      chmod 0555 "$check_provider"
      substituteInPlace "$check_provider" \
        --replace-fail '#!/usr/bin/env bash' '#!${lib.getExe bash}'
      ${lib.concatMapStringsSep "\n" (test: ''
        PATH=${lib.makeBinPath providerRuntimeInputs}:$PATH \
          ORC_PROVIDER_LIB=${./provider.sh} \
          ${lib.getExe bash} ${test} "$check_provider"
      '') checkScripts}
    '';

    installPhase = ''
      mkdir -p "$out/bin" "$out/lib" "$out/share/orc/providers/${name}"
      cp ${script} "$out/bin/orc-provider-${name}"
      cp ${./provider.sh} "$out/lib/provider.sh"
      cp ${manifest} "$out/share/orc/providers/${name}/provider.yaml"
      chmod 0555 "$out/bin/orc-provider-${name}" "$out/lib/provider.sh"
      substituteInPlace "$out/bin/orc-provider-${name}" "$out/lib/provider.sh" \
        --replace-fail '#!/usr/bin/env bash' '#!${lib.getExe bash}'
      wrapProgram "$out/bin/orc-provider-${name}" \
        --prefix PATH : ${lib.makeBinPath providerRuntimeInputs} \
        --set ORC_PROVIDER_SELF "$out/bin/orc-provider-${name}"
    '';
  };
in
symlinkJoin {
  name = "orc-provider-${name}";
  paths = [ adapter ] ++ commandPackages;
  passthru = {
    inherit adapter commandPackages;
    runtimeInputs = providerRuntimeInputs;
    providerName = name;
  };
  meta.mainProgram = "orc-provider-${name}";
}
