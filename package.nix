{
  biome,
  bun,
  bun2nix,
  makeWrapper,
  stdenv,
}:
stdenv.mkDerivation {
  pname = "orc";
  version = "0.5.0";
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
    mkdir -p \
      "$out/lib/orc" \
      "$out/bin" \
      "$out/share/bash-completion/completions" \
      "$out/share/fish/vendor_completions.d" \
      "$out/share/nushell/vendor/autoload" \
      "$out/share/zsh/site-functions"
    cp -R src node_modules package.json tsconfig.json "$out/lib/orc/"
    makeWrapper ${bun}/bin/bun "$out/bin/orc" \
      --add-flags "--preload $out/lib/orc/node_modules/@opentui/solid/scripts/preload.js" \
      --add-flags "$out/lib/orc/src/main.ts"
    "$out/bin/orc" completion bash > "$out/share/bash-completion/completions/orc"
    "$out/bin/orc" completion fish > "$out/share/fish/vendor_completions.d/orc.fish"
    "$out/bin/orc" completion nu > "$out/share/nushell/vendor/autoload/orc.nu"
    "$out/bin/orc" completion zsh > "$out/share/zsh/site-functions/_orc"
    runHook postInstall
  '';

  meta.mainProgram = "orc";
}
