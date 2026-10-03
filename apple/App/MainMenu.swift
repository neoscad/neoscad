// The menu bar. Items send standard AppKit actions down the responder
// chain (the document handles `saveDocument:`, the editor's web view
// `copy:` and `undo:`), so nothing here knows about documents or views.

import AppKit

@MainActor
enum MainMenu {
    static func make() -> NSMenu {
        let bar = NSMenu()
        bar.addItem(submenu(appMenu()))
        bar.addItem(submenu(fileMenu()))
        bar.addItem(submenu(editMenu()))
        bar.addItem(submenu(designMenu()))
        bar.addItem(submenu(viewMenu()))
        let window = windowMenu()
        bar.addItem(submenu(window))
        NSApp.windowsMenu = window
        let help = helpMenu()
        bar.addItem(submenu(help))
        NSApp.helpMenu = help
        return bar
    }

    private static func submenu(_ menu: NSMenu) -> NSMenuItem {
        let item = NSMenuItem(title: menu.title, action: nil, keyEquivalent: "")
        item.submenu = menu
        return item
    }

    private static func item(
        _ title: String, _ action: Selector?, _ key: String = "",
        _ modifiers: NSEvent.ModifierFlags = .command
    ) -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
        i.keyEquivalentModifierMask = modifiers
        return i
    }

    private static func appMenu() -> NSMenu {
        let name = ProcessInfo.processInfo.processName
        let m = NSMenu(title: name)
        m.addItem(item("About \(name)", #selector(AppDelegate.showAboutPanel(_:))))
        // Only in a build that has an updater (one with the update key);
        // Sparkle's controller is the target and enables it.
        if let updater = AppUpdater.shared {
            let check = item("Check for Updates…", AppUpdater.checkAction)
            check.target = updater.menuTarget
            m.addItem(check)
        }
        m.addItem(.separator())
        m.addItem(item("Settings…", #selector(AppDelegate.showSettings(_:)), ","))
        m.addItem(.separator())
        let services = item("Services", nil)
        services.submenu = NSMenu(title: "Services")
        NSApp.servicesMenu = services.submenu
        m.addItem(services)
        m.addItem(.separator())
        m.addItem(item("Hide \(name)", #selector(NSApplication.hide(_:)), "h"))
        m.addItem(
            item(
                "Hide Others", #selector(NSApplication.hideOtherApplications(_:)), "h",
                [.command, .option]))
        m.addItem(item("Show All", #selector(NSApplication.unhideAllApplications(_:))))
        m.addItem(.separator())
        m.addItem(item("Quit \(name)", #selector(NSApplication.terminate(_:)), "q"))
        return m
    }

    private static func fileMenu() -> NSMenu {
        let m = NSMenu(title: "File")
        m.addItem(item("New", #selector(NSDocumentController.newDocument(_:)), "n"))
        m.addItem(item("Open…", #selector(NSDocumentController.openDocument(_:)), "o"))
        // AppKit fills a submenu that holds a Clear Menu item with the
        // recent documents.
        let recent = NSMenu(title: "Open Recent")
        recent.addItem(
            item("Clear Menu", #selector(NSDocumentController.clearRecentDocuments(_:))))
        let recentItem = item("Open Recent", nil)
        recentItem.submenu = recent
        m.addItem(recentItem)
        let examples = item("Examples", nil)
        examples.submenu = ExampleMenu.shared.menu()
        m.addItem(examples)
        m.addItem(.separator())
        m.addItem(item("Close", #selector(NSWindow.performClose(_:)), "w"))
        m.addItem(item("Save…", #selector(NSDocument.save(_:)), "s"))
        m.addItem(item("Duplicate", #selector(NSDocument.duplicate(_:)), "s", [.command, .shift]))
        m.addItem(item("Rename…", #selector(NSDocument.rename(_:))))
        m.addItem(item("Move To…", #selector(NSDocument.move(_:))))
        m.addItem(item("Revert to Saved", #selector(NSDocument.revertToSaved(_:))))
        m.addItem(.separator())
        m.addItem(item("Export…", #selector(SCADDocument.exportDocument(_:)), "e", [.command, .shift]))
        return m
    }

    private static func editMenu() -> NSMenu {
        let m = NSMenu(title: "Edit")
        m.addItem(item("Undo", Selector(("undo:")), "z"))
        m.addItem(item("Redo", Selector(("redo:")), "z", [.command, .shift]))
        m.addItem(.separator())
        m.addItem(item("Cut", #selector(NSText.cut(_:)), "x"))
        m.addItem(item("Copy", #selector(NSText.copy(_:)), "c"))
        m.addItem(item("Paste", #selector(NSText.paste(_:)), "v"))
        m.addItem(item("Select All", #selector(NSText.selectAll(_:)), "a"))
        m.addItem(.separator())
        // The find bar of the text view (tag 1: show it).
        let find = item("Find…", #selector(NSTextView.performFindPanelAction(_:)), "f")
        find.tag = Int(NSFindPanelAction.showFindPanel.rawValue)
        m.addItem(find)
        return m
    }

    /// OpenSCAD's Design menu, with its keys: F5 previews, F6 renders.
    private static func designMenu() -> NSMenu {
        let m = NSMenu(title: "Design")
        let f5 = String(Character(UnicodeScalar(NSF5FunctionKey)!))
        let f6 = String(Character(UnicodeScalar(NSF6FunctionKey)!))
        m.addItem(item("Preview", #selector(SCADDocument.previewDocument(_:)), f5, []))
        m.addItem(item("Render", #selector(SCADDocument.renderDocument(_:)), f6, []))
        m.addItem(.separator())
        // NeoSCAD's `check` and `measure`, in the inspector's panels.
        m.addItem(item("Check", #selector(SCADDocument.checkDocument(_:)), "k", [.command, .shift]))
        m.addItem(item("Measure", #selector(SCADDocument.measureDocument(_:)), "m", [.command, .shift]))
        return m
    }

    /// OpenSCAD's View menu (`src/gui/MainWindow.ui`), with its shortcuts
    /// where it has them. Qt's Ctrl is the Command key on macOS, so its
    /// Ctrl+4 (Top) is Command-4 here. The grid and the lighting choice
    /// are NeoSCAD's and have no OpenSCAD shortcut.
    private static func viewMenu() -> NSMenu {
        typealias D = SCADDocument
        let m = NSMenu(title: "View")
        m.addItem(item("Show Edges", #selector(D.toggleEdges(_:)), "1"))
        m.addItem(item("Show Axes", #selector(D.toggleAxes(_:)), "2"))
        m.addItem(item("Show Crosshairs", #selector(D.toggleCrosshairs(_:)), "3"))
        m.addItem(item("Show Scale Markers", #selector(D.toggleScaleMarkers(_:))))
        m.addItem(item("Show Grid", #selector(D.toggleGrid(_:))))
        m.addItem(.separator())
        m.addItem(item("OpenSCAD Lighting", #selector(D.useOpenSCADLighting(_:))))
        m.addItem(item("Headlight", #selector(D.useHeadlight(_:))))
        m.addItem(.separator())
        m.addItem(item("Top", #selector(D.showTop(_:)), "4"))
        m.addItem(item("Bottom", #selector(D.showBottom(_:)), "5"))
        m.addItem(item("Left", #selector(D.showLeft(_:)), "6"))
        m.addItem(item("Right", #selector(D.showRight(_:)), "7"))
        m.addItem(item("Front", #selector(D.showFront(_:)), "8"))
        m.addItem(item("Back", #selector(D.showBack(_:)), "9"))
        m.addItem(item("Diagonal", #selector(D.showDiagonal(_:)), "0"))
        m.addItem(item("Center", #selector(D.centerView(_:)), "0", [.command, .shift]))
        m.addItem(.separator())
        m.addItem(item("Perspective", #selector(D.usePerspective(_:))))
        m.addItem(item("Orthogonal", #selector(D.useOrthographic(_:))))
        m.addItem(.separator())
        m.addItem(item("Reset View", #selector(D.resetView(_:))))
        m.addItem(item("View All", #selector(D.viewAll(_:)), "v", [.command, .shift]))
        m.addItem(item("Zoom In", #selector(D.zoomIn(_:)), "]"))
        m.addItem(item("Zoom Out", #selector(D.zoomOut(_:)), "["))
        m.addItem(.separator())
        // OpenSCAD's Window menu shows and hides its docks; here the
        // console's lines and the customizer are parts of the window.
        m.addItem(item("Console", #selector(D.toggleConsole(_:)), "c", [.command, .option]))
        m.addItem(item("Customizer", #selector(D.toggleCustomizer(_:)), "p", [.command, .option]))
        m.addItem(item("Check", #selector(D.showCheck(_:)), "k", [.command, .option]))
        m.addItem(item("Measure", #selector(D.showMeasure(_:)), "u", [.command, .option]))
        m.addItem(.separator())
        // The editor's font, for every window. ⌘+ is typed as ⌘= on most
        // layouts (the + is shifted), so a hidden twin answers ⌘= too.
        typealias A = AppDelegate
        m.addItem(item("Bigger", #selector(A.increaseEditorFontSize(_:)), "+"))
        let bigger = item("Bigger", #selector(A.increaseEditorFontSize(_:)), "=")
        bigger.isHidden = true
        bigger.allowsKeyEquivalentWhenHidden = true
        m.addItem(bigger)
        m.addItem(item("Smaller", #selector(A.decreaseEditorFontSize(_:)), "-"))
        m.addItem(item("Default Font Size", #selector(A.resetEditorFontSize(_:))))
        return m
    }

    /// macOS adds its search field at the top.
    private static func helpMenu() -> NSMenu {
        let m = NSMenu(title: "Help")
        m.addItem(item("Connect Your AI Agent…", #selector(AppDelegate.connectAgent(_:))))
        m.addItem(item("Using NeoSCAD with AI Agents", #selector(AppDelegate.showAgentHelp(_:))))
        return m
    }

    private static func windowMenu() -> NSMenu {
        let m = NSMenu(title: "Window")
        m.addItem(item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"))
        m.addItem(item("Zoom", #selector(NSWindow.performZoom(_:))))
        m.addItem(.separator())
        m.addItem(item("Bring All to Front", #selector(NSApplication.arrangeInFront(_:))))
        return m
    }
}
