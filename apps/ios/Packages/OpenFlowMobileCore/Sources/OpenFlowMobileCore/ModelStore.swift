import Foundation
import CryptoKit

/// Where the weights live on disk, and the proof they are the ones we pinned.
///
/// PLAN.md section 5: about 700 MB, never bundled, under Application Support with
/// `isExcludedFromBackup = true` -- iCloud should not carry a gigabyte of model
/// weights that a re-download reproduces exactly -- and checked with SHA-256
/// against a value compiled into the app.
public struct ModelStore: Sendable {
    public enum StoreError: Error, Equatable, Sendable {
        case directoryUnavailable(String)
        case fileMissing(String)
        case checksumMismatch(expected: String, actual: String)
    }

    public let directory: URL

    /// Injectable so the tests get a temporary directory.
    public init(directory: URL) {
        self.directory = directory
    }

    /// `Application Support/Models`. Application Support and not Caches: the
    /// system may purge Caches at any time, and re-downloading 700 MB because
    /// the phone wanted disk space is not a trade we want to make silently.
    public static func applicationSupport() throws -> ModelStore {
        guard let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
            throw StoreError.directoryUnavailable("No Application Support directory")
        }
        return ModelStore(directory: base.appendingPathComponent("OpenFlow/Models", isDirectory: true))
    }

    public func url(for name: String) -> URL {
        directory.appendingPathComponent(name)
    }

    public func exists(_ name: String) -> Bool {
        FileManager.default.fileExists(atPath: url(for: name).path)
    }

    public func sizeOnDisk(_ name: String) -> Int64 {
        let attributes = try? FileManager.default.attributesOfItem(atPath: url(for: name).path)
        return (attributes?[.size] as? NSNumber)?.int64Value ?? 0
    }

    /// Create the directory and mark it out of backup. Both are idempotent.
    @discardableResult
    public func prepare() throws -> URL {
        var directory = self.directory
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try directory.setResourceValues(values)
        return directory
    }

    public func isExcludedFromBackup() -> Bool {
        (try? directory.resourceValues(forKeys: [.isExcludedFromBackupKey]))?.isExcludedFromBackup ?? false
    }

    /// Streaming SHA-256. Chunked because the file is about 700 MB and reading it
    /// into memory to hash it would undo the whole point of the memory budget.
    public static func sha256Hex(ofFileAt url: URL, chunkBytes: Int = 1 << 20) throws -> String {
        guard let handle = try? FileHandle(forReadingFrom: url) else {
            throw StoreError.fileMissing(url.lastPathComponent)
        }
        defer { try? handle.close() }
        var hasher = SHA256()
        while true {
            let chunk = try handle.read(upToCount: chunkBytes) ?? Data()
            if chunk.isEmpty { break }
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// Throws `checksumMismatch` rather than returning false: a weights file that
    /// does not match the pin is not a condition to branch on quietly.
    public func verify(_ name: String, sha256Hex expected: String) throws {
        let actual = try Self.sha256Hex(ofFileAt: url(for: name))
        guard actual.caseInsensitiveCompare(expected) == .orderedSame else {
            throw StoreError.checksumMismatch(expected: expected.lowercased(), actual: actual)
        }
    }

    /// Delete a bad or unwanted download.
    public func remove(_ name: String) throws {
        let target = url(for: name)
        if FileManager.default.fileExists(atPath: target.path) {
            try FileManager.default.removeItem(at: target)
        }
    }

    /// Move a finished download into place, replacing anything already there.
    public func install(from temporary: URL, as name: String) throws {
        try prepare()
        let destination = url(for: name)
        if FileManager.default.fileExists(atPath: destination.path) {
            try FileManager.default.removeItem(at: destination)
        }
        try FileManager.default.moveItem(at: temporary, to: destination)
    }
}
