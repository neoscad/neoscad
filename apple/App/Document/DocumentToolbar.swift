// The document window's toolbar: Preview and Render, and at the trailing
// end the AI agent control (Agents/AgentControls.swift). Kept short on
// purpose; the compact style keeps the title in the same row, so the
// editor and the view lose almost no height to it.
//
// The agent control hides once the user has turned agents off (in Settings
// or the sheet), as the audit's UI spec has it; the Help menu still opens
// the sheet. Its visibility follows the service through Observation, not a
// timer.

import AppKit
import SwiftUI

@MainActor
final class DocumentToolbar: NSObject, NSToolbarDelegate {
    static let preview = NSToolbarItem.Identifier("org.neoscad.toolbar.preview")
    static let render = NSToolbarItem.Identifier("org.neoscad.toolbar.render")
    static let agent = NSToolbarItem.Identifier("org.neoscad.toolbar.agent")

    private(set) var agentItem: NSToolbarItem?

    func install(in window: NSWindow) {
        let toolbar = NSToolbar(identifier: "org.neoscad.document")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = false
        window.toolbar = toolbar
        window.toolbarStyle = .unifiedCompact
        followAgentVisibility()
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [Self.preview, Self.render, .flexibleSpace, Self.agent]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar)
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier id: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        switch id {
        case Self.preview:
            return button(
                id, "Preview", "play", "Preview the model (F5)",
                #selector(SCADDocument.previewDocument(_:)))
        case Self.render:
            return button(
                id, "Render", "cube", "Render the model (F6)",
                #selector(SCADDocument.renderDocument(_:)))
        case Self.agent:
            let item = NSToolbarItem(itemIdentifier: id)
            item.label = "AI Agent"
            item.paletteLabel = "AI Agent"
            item.visibilityPriority = .high
            let host = NSHostingView(rootView: AgentToolbarControl(service: .shared))
            host.sizingOptions = [.intrinsicContentSize]
            item.view = host
            item.isHidden = AgentService.shared.turnedOff
            agentItem = item
            return item
        default:
            return nil
        }
    }

    private func button(
        _ id: NSToolbarItem.Identifier, _ label: String, _ symbol: String, _ tip: String,
        _ action: Selector
    ) -> NSToolbarItem {
        let item = NSToolbarItem(itemIdentifier: id)
        item.label = label
        item.paletteLabel = label
        item.toolTip = tip
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        item.isBordered = true
        // Down the responder chain to the window's document.
        item.action = action
        return item
    }

    /// Hide the agent control while agents are turned off, and show it
    /// again when they are turned on.
    private func followAgentVisibility() {
        let off = withObservationTracking {
            AgentService.shared.turnedOff
        } onChange: { [weak self] in
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.followAgentVisibility() }
            }
        }
        agentItem?.isHidden = off
    }
}
