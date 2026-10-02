// Sparkle EdDSA keys without the keychain (docs/release.md, "The macOS
// app's updates"):
//
//   xcrun swift scripts/release/sparkle-key.swift public < PRIVATE_KEY_FILE
//   xcrun swift scripts/release/sparkle-key.swift generate PRIVATE_KEY_FILE
//
// `public` prints the public key (base64, what goes in apple/project.yml's
// NEOSCAD_SPARKLE_PUBLIC_KEY) for a private key file's text: the base64
// 32-byte seed that Sparkle's `generate_keys -x` exports and the secret
// SPARKLE_ED_PRIVATE_KEY holds. update-feed.yml uses it to refuse a secret
// that the apps don't trust.
//
// `generate` writes a new private key file (mode 600) and prints its public
// key. It is for throwaway test keys (scripts/apple/test-updates.sh): the
// real key is made once with Sparkle's own `generate_keys`, kept in the
// owner's password manager, and never touches this repository.
//
// Sparkle's seed is an RFC 8032 Ed25519 seed, which is what CryptoKit's
// `rawRepresentation` is, so both derive the same public key.

import CryptoKit
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("sparkle-key: \(message)\n".utf8))
    exit(1)
}

let args = CommandLine.arguments.dropFirst()
switch (args.first, args.count) {
case ("public", 1):
    let text = String(decoding: FileHandle.standardInput.readDataToEndOfFile(), as: UTF8.self)
        .trimmingCharacters(in: .whitespacesAndNewlines)
    guard let seed = Data(base64Encoded: text) else { fail("the private key is not base64") }
    // Keys from Sparkle before 2.0's format change are 96 bytes (a hashed
    // seed and the public key); a new key is always the 32-byte seed.
    guard seed.count == 32 else {
        fail("the private key is \(seed.count) bytes, not a 32-byte seed (make it with Sparkle 2's generate_keys)")
    }
    guard let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: seed) else {
        fail("not an Ed25519 seed")
    }
    print(key.publicKey.rawRepresentation.base64EncodedString())
case ("generate", 2):
    let path = args[args.startIndex + 1]
    guard !FileManager.default.fileExists(atPath: path) else { fail("\(path) exists") }
    let key = Curve25519.Signing.PrivateKey()
    let text = key.rawRepresentation.base64EncodedString() + "\n"
    guard FileManager.default.createFile(
        atPath: path, contents: Data(text.utf8), attributes: [.posixPermissions: 0o600])
    else { fail("cannot write \(path)") }
    print(key.publicKey.rawRepresentation.base64EncodedString())
default:
    fail("usage: sparkle-key.swift public < KEY_FILE | generate KEY_FILE")
}
