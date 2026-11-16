# NOTE: The release job of .github/workflows/ci.yml sets `version` and `sha256` on every tag, in a commit of its own.
cask "ooblin" do
  version "0.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/kevinvalk/ooblin/releases/download/v#{version}/Ooblin_#{version}_universal.dmg"
  name "Ooblin"
  desc "Console for Intel AMT, Supermicro BMCs and any BMC over IPMI or Redfish"
  homepage "https://github.com/kevinvalk/ooblin"

  livecheck do
    url :url
    strategy :github_latest
  end

  auto_updates true
  depends_on :macos

  app "Ooblin.app"

  zap trash: [
    "~/Library/Application Support/nl.kevinvalk.ooblin",
    "~/Library/Caches/nl.kevinvalk.ooblin",
    "~/Library/Logs/nl.kevinvalk.ooblin",
    "~/Library/Preferences/nl.kevinvalk.ooblin.plist",
    "~/Library/Saved Application State/nl.kevinvalk.ooblin.savedState",
    "~/Library/WebKit/nl.kevinvalk.ooblin",
  ]

  # TODO: Once Apple signs and notarizes Ooblin (see .github/workflows/ci.yml), this goes.
  caveats <<~EOS
    Ooblin is not signed by Apple yet, so macOS refuses to open it the first time.
    Allow it once in System Settings > Privacy & Security > Open Anyway.
  EOS
end
