// The offer to be the default app for .scad files (Document/DefaultApp.swift).
// Every test answers LaunchServices through a fake workspace: nothing here
// reads or changes this Mac's real default, and setDefaultApplication is
// never called for real, since on macOS it puts a system confirmation in
// front of whoever is at the Mac.

import AppKit
import Foundation
import Testing
import UniformTypeIdentifiers

@testable import NeoSCAD

@MainActor
private final class FakeWorkspace: DefaultAppWorkspace {
    var defaultIdentifier: String?
    var defaultName: String?
    /// What the "system" answers: `nil` makes NeoSCAD the default.
    var setError: Error?
    private(set) var setCalls: [(URL, UTType)] = []

    init(defaultIdentifier: String? = nil) {
        self.defaultIdentifier = defaultIdentifier
    }

    func defaultApplicationIdentifier(for type: UTType) -> String? { defaultIdentifier }
    func defaultApplicationName(for type: UTType) -> String? { defaultName }

    func setDefaultApplication(at application: URL, for type: UTType) async throws {
        setCalls.append((application, type))
        // The answer comes later, after the system's confirmation.
        try await Task.sleep(for: .milliseconds(5))
        if let setError { throw setError }
        defaultIdentifier = DefaultAppTests.ownID
    }
}

@MainActor
struct DefaultAppTests {
    static let ownID = "org.neoscad.NeoSCAD"

    /// An installed release build: the only kind that asks.
    private func installed(
        _ path: String = "/Applications/NeoSCAD.app", debug: Bool = false, tests: Bool = false
    ) -> DefaultApp.Context {
        DefaultApp.Context(
            bundleURL: URL(fileURLWithPath: path), bundleIdentifier: Self.ownID,
            isDebugBuild: debug, underTests: tests)
    }

    private func withDefaults(_ name: String, _ body: (UserDefaults) throws -> Void) throws {
        let suite = "org.neoscad.NeoSCADAppTests.defaultApp.\(name)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        try body(defaults)
    }

    @Test func asksAnInstalledReleaseBuildThatIsNotTheDefault() throws {
        try withDefaults("asks") { d in
            #expect(DefaultApp.reasonNotToAsk(installed(), defaults: d, workspace: FakeWorkspace()) == nil)
            let other = FakeWorkspace(defaultIdentifier: "org.openscad.OpenSCAD")
            #expect(DefaultApp.reasonNotToAsk(installed(), defaults: d, workspace: other) == nil)
        }
    }

    /// Compared by bundle identifier: another copy of NeoSCAD being the
    /// default counts.
    @Test func doesNotAskWhenAlreadyTheDefault() throws {
        try withDefaults("already") { d in
            let ws = FakeWorkspace(defaultIdentifier: Self.ownID)
            #expect(DefaultApp.reasonNotToAsk(installed(), defaults: d, workspace: ws) != nil)
            #expect(DefaultApp.isDefault(installed("/opt/elsewhere/Applications/NeoSCAD.app"), workspace: ws))
        }
    }

    @Test func doesNotAskAfterDontAskAgain() throws {
        try withDefaults("suppressed") { d in
            d.set(true, forKey: DefaultApp.dontAskKey)
            #expect(DefaultApp.reasonNotToAsk(installed(), defaults: d, workspace: FakeWorkspace()) != nil)
        }
    }

    @Test func doesNotAskFromDebugTestOrTemporaryCopies() throws {
        try withDefaults("where") { d in
            let ws = FakeWorkspace()
            #expect(DefaultApp.reasonNotToAsk(installed(debug: true), defaults: d, workspace: ws) != nil)
            #expect(DefaultApp.reasonNotToAsk(installed(tests: true), defaults: d, workspace: ws) != nil)
            let derived = installed("/opt/src/apple/build/DerivedData/Build/Products/Release/NeoSCAD.app")
            #expect(DefaultApp.reasonNotToAsk(derived, defaults: d, workspace: ws) != nil)
            let translocated = installed("/var/folders/xy/T/AppTranslocation/1234/d/NeoSCAD.app")
            #expect(DefaultApp.reasonNotToAsk(translocated, defaults: d, workspace: ws) != nil)
            #expect(DefaultApp.reasonNotToRegister(appAt: translocated.bundleURL) != nil)
            #expect(ws.setCalls.isEmpty)
        }
    }

    /// This test host: a Debug build in DerivedData under XCTest, which
    /// must never ask.
    @Test func theTestHostNeverAsks() throws {
        try withDefaults("host") { d in
            let context = DefaultApp.Context.current
            #expect(context.underTests)
            #expect(context.isDebugBuild)
            #expect(DefaultApp.reasonNotToAsk(context, defaults: d, workspace: FakeWorkspace()) != nil)
        }
    }

    @Test func makeDefaultReportsSuccess() async {
        let ws = FakeWorkspace(defaultIdentifier: "org.openscad.OpenSCAD")
        let failure: String? = await withCheckedContinuation { c in
            DefaultApp.makeDefault(installed(), workspace: ws) { c.resume(returning: $0) }
        }
        #expect(failure == nil)
        #expect(ws.setCalls.count == 1)
        #expect(ws.setCalls.first?.0.path == "/Applications/NeoSCAD.app")
        #expect(ws.setCalls.first?.1.identifier == "org.openscad.scad")
    }

    /// The user's "Don't Change" in the system's confirmation: reported
    /// once, never retried.
    @Test func makeDefaultReportsADeclinedChange() async {
        let ws = FakeWorkspace(defaultIdentifier: "org.openscad.OpenSCAD")
        ws.setError = NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError)
        let failure: String? = await withCheckedContinuation { c in
            DefaultApp.makeDefault(installed(), workspace: ws) { c.resume(returning: $0) }
        }
        #expect(failure != nil)
        #expect(ws.setCalls.count == 1)
        #expect(!DefaultApp.isDefault(installed(), workspace: ws))
    }

    /// A temporary copy isn't registered even when Settings asks.
    @Test func makeDefaultRefusesATranslocatedCopy() async {
        let ws = FakeWorkspace()
        let context = installed("/var/folders/xy/T/AppTranslocation/1234/d/NeoSCAD.app")
        let failure: String? = await withCheckedContinuation { c in
            DefaultApp.makeDefault(context, workspace: ws) { c.resume(returning: $0) }
        }
        #expect(failure != nil)
        #expect(ws.setCalls.isEmpty)
    }

    @Test func settingsRowShowsTheStateAndMakesDefault() async throws {
        let ws = FakeWorkspace(defaultIdentifier: "org.openscad.OpenSCAD")
        ws.defaultName = "OpenSCAD"
        let model = DefaultAppSettingsModel(context: installed(), workspace: ws)
        #expect(!model.isDefault)
        #expect(model.currentName == "OpenSCAD")
        #expect(model.cannotRegister == nil)
        model.makeDefault()
        for _ in 0..<100 where model.working {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.isDefault)
        #expect(model.failure == nil)
        #expect(ws.setCalls.count == 1)
    }

    /// Info.plist declares the type the offer is about.
    @Test func infoPlistImportsTheScadType() throws {
        let info = try #require(Bundle.main.infoDictionary)
        let imported = try #require(info["UTImportedTypeDeclarations"] as? [[String: Any]])
        #expect(imported.contains { $0["UTTypeIdentifier"] as? String == DefaultApp.scadType.identifier })
    }
}
