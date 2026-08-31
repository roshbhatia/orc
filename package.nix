{
  biome,
  bun2nix,
  stdenv,
}:
bun2nix.mkDerivation {
  pname = "example";
  version = "0.1.0";
  src = ./.;
  module = "src/main.ts";

  nativeBuildInputs = [ biome ];
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
  checkPhase = ''
    runHook preCheck
    biome check .
    bun run typecheck
    bun test
    runHook postCheck
  '';
}
