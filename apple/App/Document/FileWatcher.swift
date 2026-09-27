// Watches the files a document's last run read (its includes, used
// libraries, imports and fonts; `DocumentResult.files`) and says when one
// of them changes on disk, so the document runs again, as OpenSCAD's
// "Automatic Reload and Preview" does for the files a design depends on.
//
// FSEvents on the files' directories, with file-level events: an editor
// that saves by writing a temporary file and renaming it over the original
// (most do) replaces the file's inode, which a watcher of the old file
// descriptor (DispatchSource) would lose after the first save; a
// directory stream sees the new file under the same path. Paths are
// compared with symlinks resolved, because FSEvents reports real paths
// (`/private/var/...`) while the core names files as they were reached
// (`/var/...`).

import CoreServices
import Foundation

@MainActor
final class FileWatcher {
    /// Called on the main thread after a watched file changed; changes
    /// within `latency` come as one call.
    var onChange: (() -> Void)?

    /// The files watched (real paths).
    private(set) var files: Set<String> = []
    private var directories: [String] = []
    /// Unsafe only so `deinit` can release it: every other use is on the
    /// main actor, and the watcher goes away on it too.
    nonisolated(unsafe) private var stream: FSEventStreamRef?

    /// What the stream's callback holds: the watcher, weakly, so a stream
    /// that outlived its watcher calls nothing.
    private final class Box {
        weak var watcher: FileWatcher?
        init(_ w: FileWatcher) { watcher = w }
    }

    /// FSEvents coalesces events within this interval.
    static let latency: CFTimeInterval = 0.1

    /// Watch exactly `paths` from now on (paths that do not exist are
    /// skipped).
    func watch(_ paths: [String]) {
        let real = Set(paths.map { URL(fileURLWithPath: $0).resolvingSymlinksInPath().path })
        guard real != files else { return }
        files = real
        let dirs = Array(Set(real.map { ($0 as NSString).deletingLastPathComponent })).sorted()
        guard dirs != directories else { return }
        directories = dirs
        restart()
    }

    /// Stop watching (the document closed).
    func stop() {
        files = []
        directories = []
        if let stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
        stream = nil
    }

    private func restart() {
        if let stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
            self.stream = nil
        }
        guard !directories.isEmpty else { return }
        var context = FSEventStreamContext(
            version: 0, info: Unmanaged.passRetained(Box(self)).toOpaque(),
            retain: nil,
            release: { info in
                if let info { Unmanaged<Box>.fromOpaque(info).release() }
            },
            copyDescription: nil)
        let callback: FSEventStreamCallback = { _, info, count, paths, _, _ in
            guard let info else { return }
            let box = Unmanaged<Box>.fromOpaque(info).takeUnretainedValue()
            let list = Unmanaged<CFArray>.fromOpaque(paths).takeUnretainedValue() as? [String] ?? []
            MainActor.assumeIsolated {
                box.watcher?.received(Array(list.prefix(count)))
            }
        }
        let flags = UInt32(
            kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes
                | kFSEventStreamCreateFlagNoDefer)
        guard
            let s = FSEventStreamCreate(
                nil, callback, &context, directories as CFArray,
                FSEventStreamEventId(kFSEventStreamEventIdSinceNow), Self.latency, flags)
        else {
            // The stream never took the box.
            if let info = context.info { Unmanaged<Box>.fromOpaque(info).release() }
            return
        }
        // Delivered on the main queue, which is where `received` runs.
        FSEventStreamSetDispatchQueue(s, DispatchQueue.main)
        FSEventStreamStart(s)
        stream = s
    }

    private func received(_ paths: [String]) {
        let hit = paths.contains { files.contains(URL(fileURLWithPath: $0).resolvingSymlinksInPath().path) }
        if hit { onChange?() }
    }

    deinit {
        if let stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
    }
}
