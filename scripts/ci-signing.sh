#!/usr/bin/env bash
# Sets up signing on a GitHub Actions runner from the release environment's secrets: imports the
# Developer ID certificate into a throwaway keychain and writes the notary and Sparkle keys to
# files, exporting their paths to the later steps through $GITHUB_ENV.
#
#   DEVELOPER_ID_P12           the certificate and its private key, as base64 of a .p12 file
#   DEVELOPER_ID_P12_PASSWORD  the .p12's password
#   NOTARY_KEY                 the App Store Connect API key (.p8 contents)
#   SPARKLE_KEY                the Sparkle private key (from `generate_keys --account annuncia -x`)
set -euo pipefail

keychain="$RUNNER_TEMP/signing.keychain-db"
password="$(uuidgen)"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
printf '%s' "$DEVELOPER_ID_P12" | base64 --decode > "$RUNNER_TEMP/developer-id.p12"
security import "$RUNNER_TEMP/developer-id.p12" -k "$keychain" -P "$DEVELOPER_ID_P12_PASSWORD" -T /usr/bin/codesign
rm "$RUNNER_TEMP/developer-id.p12"
# Lets codesign use the key without a prompt nobody is there to answer.
security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null
# shellcheck disable=SC2046
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')

printf '%s' "$NOTARY_KEY" > "$RUNNER_TEMP/notary.p8"
printf '%s' "$SPARKLE_KEY" > "$RUNNER_TEMP/sparkle.key"
{
  echo "NOTARY_KEY_PATH=$RUNNER_TEMP/notary.p8"
  echo "SPARKLE_KEY_FILE=$RUNNER_TEMP/sparkle.key"
} >> "$GITHUB_ENV"
