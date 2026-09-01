{
  biome,
  bun,
  bun2nix,
  makeWrapper,
  stdenv,
}:
stdenv.mkDerivation {
  pname = "orc";
  version = "0.4.1";
  src = ./.;

  nativeBuildInputs = [
    biome
    bun2nix.hook
    makeWrapper
  ];
  bunDeps = bun2nix.fetchBunDeps { bunNix = ./bun.nix; };
  bunInstallFlags =
    if stdenv.hostPlatform.isDarwin then
      [
        "--linker=hoisted"
        "--backend=copyfile"
      ]
    else
      [ "--linker=hoisted" ];

  doCheck = true;
  dontStrip = true;
  dontUseBunBuild = true;
  dontUseBunInstall = true;
  checkPhase = ''
    runHook preCheck
    biome check .
    bun run typecheck
    bun test --path-ignore-patterns 'result/**'
    runHook postCheck
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out/lib/orc" "$out/bin" "$out/share/fish/vendor_completions.d"
    cp -R src node_modules package.json tsconfig.json "$out/lib/orc/"
    makeWrapper ${bun}/bin/bun "$out/bin/orc" \
      --add-flags "--preload $out/lib/orc/node_modules/@opentui/solid/scripts/preload.js" \
      --add-flags "$out/lib/orc/src/main.ts"
    "$out/bin/orc" completion fish > "$out/share/fish/vendor_completions.d/orc.fish"
    runHook postInstall
  '';

  meta.mainProgram = "orc";
}
