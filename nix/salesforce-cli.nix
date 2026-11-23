# The Salesforce CLI, with which the Salesforce connector's org tests prepare
# their org, from Salesforce's release tarballs: nixpkgs does not provide it.
# Each tarball bundles the Node.js it runs on. The hashes are those of the
# channel's build manifest
# (https://developer.salesforce.com/media/salesforce-cli/sf/channels/stable/sf-<platform>-buildmanifest).
{
  lib,
  stdenv,
  fetchurl,
  makeWrapper,
  autoPatchelfHook,
}:
let
  version = "2.158.7";
  commit = "efd8c9d";
  tarball =
    {
      "aarch64-darwin" = {
        platform = "darwin-arm64";
        sha256 = "6a99fec0b278c3a4e0888ec7d7bb44cbe7f76cf0729f76f662fba6b54ea9c6b9";
      };
      "x86_64-darwin" = {
        platform = "darwin-x64";
        sha256 = "840f56df67d42fc2f3376151bfbb6499b5c5b4a5a22c2fe153c292e6defd39e8";
      };
      "aarch64-linux" = {
        platform = "linux-arm64";
        sha256 = "3a7b39e7eff16223d43ee4380725f108e692abb099a3d6976f6186e0047712c4";
      };
      "x86_64-linux" = {
        platform = "linux-x64";
        sha256 = "a03d906c584a79a632bc9ddafc5536b588fa507874555a6d10612d5f15ff71d8";
      };
    }
    .${stdenv.hostPlatform.system};
in
stdenv.mkDerivation {
  pname = "salesforce-cli";
  inherit version;
  src = fetchurl {
    url = "https://developer.salesforce.com/media/salesforce-cli/sf/versions/${version}/${commit}/sf-v${version}-${commit}-${tarball.platform}.tar.xz";
    inherit (tarball) sha256;
  };
  # The bundled Node.js needs libstdc++ but carries no RUNPATH, and names the
  # host's dynamic loader.
  nativeBuildInputs = [ makeWrapper ] ++ lib.optionals stdenv.isLinux [ autoPatchelfHook ];
  buildInputs = lib.optionals stdenv.isLinux [ stdenv.cc.cc.lib ];
  dontBuild = true;
  # The macOS Node.js is used as shipped, with its signature.
  dontFixup = stdenv.isDarwin;
  # The CLI would otherwise update itself into the user's data directory and
  # run that version instead of the pinned one.
  installPhase = ''
    mkdir -p $out/libexec
    cp -R . $out/libexec/sf
    makeWrapper $out/libexec/sf/bin/sf $out/bin/sf \
      --set SF_AUTOUPDATE_DISABLE true \
      --set SF_DISABLE_AUTOUPDATE true
  '';
}
