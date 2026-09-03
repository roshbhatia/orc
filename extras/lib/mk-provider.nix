{
  lib,
  makeWrapper,
  shellcheck,
  stdenvNoCC,
  symlinkJoin,
}:
{
  commandPackages ? [ ],
  manifest,
  name,
  runtimeInputs,
  script,
}:
let
  adapter = stdenvNoCC.mkDerivation {
    pname = "orc-provider-${name}-adapter";
    version = "0.10.0";
    dontUnpack = true;
    strictDeps = true;

    nativeBuildInputs = [
      makeWrapper
      shellcheck
    ];

    doCheck = true;
    checkPhase = ''
      shellcheck -x -P ${../.} ${script}
    '';

    installPhase = ''
      mkdir -p "$out/bin" "$out/lib" "$out/share/orc/providers/${name}"
      cp ${script} "$out/bin/orc-provider-${name}"
      cp ${./provider.sh} "$out/lib/provider.sh"
      cp ${manifest} "$out/share/orc/providers/${name}/provider.yaml"
      chmod 0555 "$out/bin/orc-provider-${name}" "$out/lib/provider.sh"
      patchShebangs "$out/bin/orc-provider-${name}"
      wrapProgram "$out/bin/orc-provider-${name}" \
        --prefix PATH : ${lib.makeBinPath runtimeInputs}
    '';
  };
in
symlinkJoin {
  name = "orc-provider-${name}";
  paths = [ adapter ] ++ commandPackages;
  passthru = {
    inherit adapter commandPackages runtimeInputs;
    providerName = name;
  };
  meta.mainProgram = "orc-provider-${name}";
}
