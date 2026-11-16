import AppKit
import CoreGraphics
import Darwin
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers
import Vision

struct SensorError: LocalizedError {
    let errorDescription: String?

    init(_ message: String) {
        errorDescription = message
    }
}

struct Region: Decodable {
    let x: Double
    let y: Double
    let width: Double
    let height: Double

    var isValid: Bool {
        x >= 0 && y >= 0 && width > 0 && height > 0 && x + width <= 1 && y + height <= 1
    }

    func rect(in image: CGImage) -> CGRect {
        CGRect(x: x * Double(image.width), y: y * Double(image.height),
               width: width * Double(image.width), height: height * Double(image.height)).integral
    }
}

struct FieldConfig: Decodable {
    let id: String
    let region: Region
    let pattern: String?
    let confirmations: Int?
    let ocrScale: Int?
    let accurateOCR: Bool?
    let matchAll: Bool?
    let validateCounter: Bool?
    let rejectHorizontalEdgeText: Bool?
}

struct Profile: Decodable {
    let id: String
    let intervalMs: Int
    let fields: [FieldConfig]
}

struct FieldState: Encodable {
    let id: String
    let status: String
    let value: String?
    let candidate: String?
    let valueFrame: Int?
    let confidence: Float?
    let ocrReused: Bool
    let rows: [OCRRow]
    let possibleClipping: Bool
    let carried: Bool
    let heldSamples: Int?
    let heldMs: Int64?
    let confirmationSamples: Int
}

struct Change: Encodable {
    let field: String
    let previous: String?
    let value: String
}

struct Observation: Encodable {
    let schema = "boxen.visual.v1"
    let session: String
    let profile: String
    let frame: Int
    let capturedAtUnixMs: Int64
    let width: Int
    let height: Int
    let captureMs: Double
    let processingMs: Double
    let evidenceRetained: Bool
    let fields: [FieldState]
    let changes: [Change]
}

struct OCRRow: Encodable {
    struct Bounds: Encodable {
        let x: Double
        let y: Double
        let width: Double
        let height: Double
    }

    let text: String
    let bounds: Bounds
    let confidence: Float
    let possibleClipping: Bool
}

struct Reading {
    let value: String?
    let rows: [OCRRow]

    var confidence: Float? { rows.map(\.confidence).min() }
}

struct Field {
    let config: FieldConfig
    let pattern: NSRegularExpression?
    var lastPixels: Data?
    var lastReading: Reading?
    var pending: String?
    var consecutive = 0
    var value: String?
    var valueFrame: Int?
    var valueAtMs: Int64?

    init(_ config: FieldConfig) throws {
        guard config.region.isValid, !config.id.isEmpty, config.id.utf8.count <= 64,
              (1...20).contains(config.confirmations ?? 1),
              (1...4).contains(config.ocrScale ?? 1),
              (config.pattern?.utf8.count ?? 0) <= 512 else {
            throw SensorError("Invalid field ID, region, pattern size, or confirmation count.")
        }
        self.config = config
        pattern = try config.pattern.map { try NSRegularExpression(pattern: $0) }
    }

    mutating func process(_ image: CGImage, frame: Int, capturedAtMs: Int64) throws -> (FieldState, Change?) {
        guard let crop = image.cropping(to: config.region.rect(in: image)) else {
            throw SensorError("Field region could not be cropped.")
        }

        let pixels = pixelBytes(crop)
        let reused = pixels != nil && pixels == lastPixels
        if !reused {
            lastReading = try recognize(crop)
            lastPixels = pixels
        }

        var change: Change?
        if let reading = lastReading?.value {
            if reading == pending {
                consecutive = min(consecutive + 1, config.confirmations ?? 1)
            } else {
                pending = reading
                consecutive = 1
            }

            if consecutive >= (config.confirmations ?? 1) {
                if reading != value {
                    change = Change(field: config.id, previous: value, value: reading)
                }
                value = reading
                valueFrame = frame
                valueAtMs = capturedAtMs
            }
        } else {
            pending = nil
            consecutive = 0
        }

        let status: String
        if lastReading?.value == nil {
            status = "unreadable"
        } else if valueFrame != frame {
            status = "confirming"
        } else {
            status = "observed"
        }

        let rows = lastReading?.rows ?? []
        return (FieldState(id: config.id, status: status, value: value,
                           candidate: status == "confirming" ? pending : nil, valueFrame: valueFrame,
                           confidence: lastReading?.confidence, ocrReused: reused,
                           rows: rows, possibleClipping: rows.contains(where: \.possibleClipping),
                           carried: valueFrame.map { $0 != frame } ?? false,
                           heldSamples: valueFrame.map { frame - $0 },
                           heldMs: valueAtMs.map { max(0, capturedAtMs - $0) },
                           confirmationSamples: consecutive), change)
    }

    private func recognize(_ crop: CGImage) throws -> Reading? {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = config.accurateOCR == true ? .accurate : .fast
        request.usesLanguageCorrection = false
        try VNImageRequestHandler(cgImage: ocrImage(crop)).perform([request])
        // Vision uses a lower-left origin. Published bounds are crop-relative with an upper-left origin.
        let rows = (request.results ?? []).prefix(24).compactMap { observation -> OCRRow? in
            guard let candidate = observation.topCandidates(1).first else { return nil }
            let box = observation.boundingBox
            return OCRRow(text: String(candidate.string.unicodeScalars.prefix(96)),
                          bounds: .init(x: box.minX, y: 1 - box.maxY, width: box.width, height: box.height),
                          confidence: candidate.confidence,
                          possibleClipping: box.minX <= 0.02 || box.maxX >= 0.98)
        }
        let included = rows.filter { config.rejectHorizontalEdgeText != true || !$0.possibleClipping }
        let text = String(included.map(\.text).joined(separator: "\n").unicodeScalars.prefix(2048))
        guard let pattern else { return Reading(value: text.isEmpty ? nil : text, rows: rows) }
        let textRange = NSRange(text.startIndex..., in: text)
        let matches = config.matchAll == true
            ? pattern.matches(in: text, range: textRange)
            : [pattern.firstMatch(in: text, range: textRange)].compactMap { $0 }
        let values = matches.compactMap { match -> String? in
            let matchRange = match.range(at: match.numberOfRanges > 1 ? 1 : 0)
            guard let range = Range(matchRange, in: text), !range.isEmpty else { return nil }
            let value = String(text[range])
            if config.validateCounter == true {
                let parts = value.split(separator: "/", maxSplits: 1)
                guard parts.count == 2,
                      let done = Int(parts[0].trimmingCharacters(in: .whitespaces)),
                      let total = Int(parts[1].trimmingCharacters(in: .whitespaces).prefix(while: \.isNumber)),
                      total > 0, done <= total else { return nil }
            }
            return value
        }
        return Reading(value: values.isEmpty ? nil : values.joined(separator: "\n"), rows: rows)
    }

    private func ocrImage(_ image: CGImage) -> CGImage {
        let scale = min(config.ocrScale ?? 1, 1920 / max(image.width, image.height))
        guard scale > 1 else { return image }
        let width = image.width * scale
        let height = image.height * scale
        guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                                      bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return image }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage() ?? image
    }

    // Compare the exact rendered RGBA bytes. One changed pixel invalidates the OCR cache.
    private func pixelBytes(_ image: CGImage) -> Data? {
        let rowBytes = image.width * 4
        guard let context = CGContext(data: nil, width: image.width, height: image.height,
                                      bitsPerComponent: 8, bytesPerRow: rowBytes,
                                      space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let bytes = context.data else { return nil }
        context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        return Data(bytes: bytes, count: rowBytes * image.height)
    }
}

struct Archive {
    let directory: URL
    let capacity: Int

    func store(_ image: CGImage, frame: Int) throws {
        let output = directory.appendingPathComponent("\(frame).png")
        let temporary = directory.appendingPathComponent(".frame.tmp")
        try writePNG(image, to: temporary)
        try FileManager.default.moveItem(at: temporary, to: output)
        if frame > capacity {
            try FileManager.default.removeItem(at: directory.appendingPathComponent("\(frame - capacity).png"))
        }
    }
}

func writePNG(_ image: CGImage, to url: URL) throws {
    guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else {
        throw SensorError("Image destination could not be opened.")
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { throw SensorError("Image could not be written.") }
}

func readImage(_ url: URL) throws -> CGImage {
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
        throw SensorError("Image is unavailable or unreadable.")
    }
    return image
}

func loadProfile(_ path: String) throws -> Profile {
    let data = try FileHandle(forReadingFrom: URL(fileURLWithPath: path)).read(upToCount: 65_537) ?? Data()
    guard data.count <= 65_536 else { throw SensorError("Profile exceeds 64 KiB.") }
    let profile = try JSONDecoder().decode(Profile.self, from: data)
    guard !profile.id.isEmpty, profile.id.utf8.count <= 64, (100...60_000).contains(profile.intervalMs),
          (1...8).contains(profile.fields.count),
          Set(profile.fields.map(\.id)).count == profile.fields.count else {
        throw SensorError("Invalid profile ID, interval, field count, or duplicate field IDs.")
    }
    return profile
}

struct Sensor {
    let profile: Profile
    let session = UUID().uuidString.lowercased()
    var fields: [Field]
    var archive: Archive?
    let encoder = JSONEncoder()

    init(profile: Profile, archivePath: String?) throws {
        self.profile = profile
        fields = try profile.fields.map(Field.init)
        encoder.outputFormatting = [.sortedKeys]
        if let archivePath {
            let directory = URL(fileURLWithPath: archivePath).appendingPathComponent(session, isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                                                    attributes: [.posixPermissions: 0o700])
            archive = Archive(directory: directory, capacity: 12)
        }
    }

    mutating func observe(_ image: CGImage, frame: Int, capturedAt: Date, captureMs: Double) throws -> Observation {
        let start = DispatchTime.now().uptimeNanoseconds
        let capturedAtMs = Int64(capturedAt.timeIntervalSince1970 * 1000)
        var states: [FieldState] = []
        var changes: [Change] = []
        for index in fields.indices {
            let (state, change) = try fields[index].process(image, frame: frame, capturedAtMs: capturedAtMs)
            states.append(state)
            if let change { changes.append(change) }
        }
        let processingMs = elapsedMs(since: start)
        try archive?.store(image, frame: frame)
        return Observation(session: session, profile: profile.id, frame: frame,
                                      capturedAtUnixMs: capturedAtMs,
                                      width: image.width, height: image.height,
                                      captureMs: captureMs, processingMs: processingMs,
                                      evidenceRetained: archive != nil, fields: states, changes: changes)
    }

    mutating func accept(_ image: CGImage, frame: Int, capturedAt: Date, captureMs: Double) throws {
        let observation = try observe(image, frame: frame, capturedAt: capturedAt, captureMs: captureMs)
        var line = try encoder.encode(observation)
        line.append(10)
        try FileHandle.standardOutput.write(contentsOf: line)
    }
}

struct WatchSample: Encodable {
    let observation: Observation
    let changes: [Change]
    let fresh: Bool
    let images: [WatchImage]
}

struct WatchImage: Encodable {
    // A missing field denotes the whole capture.
    let field: String?
    let size: Int
}

func pngBytes(_ image: CGImage) -> Data? {
    let bytes = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(bytes, UTType.png.identifier as CFString, 1, nil)
    else { return nil }
    CGImageDestinationAddImage(destination, image, nil)
    return CGImageDestinationFinalize(destination) ? bytes as Data : nil
}

func writeWatchSample(_ observation: Observation, image: CGImage, profile: Profile,
                      watched: Set<String>, whole: Bool, fresh: Bool, encoder: JSONEncoder) throws {
    let changes = observation.changes.filter { $0.previous != nil && watched.contains($0.field) }
    var images: [(WatchImage, Data)] = []
    var remaining = 16 * 1024 * 1024
    func append(_ image: CGImage, field: String?) {
        guard let bytes = pngBytes(image), bytes.count <= remaining else { return }
        images.append((WatchImage(field: field, size: bytes.count), bytes))
        remaining -= bytes.count
    }
    if fresh || !changes.isEmpty {
        for field in profile.fields {
            if let crop = image.cropping(to: field.region.rect(in: image)) {
                append(crop, field: field.id)
            }
        }
        if whole { append(image, field: nil) }
    }
    let sample = WatchSample(observation: observation, changes: changes, fresh: fresh, images: images.map(\.0))
    var line = try encoder.encode(sample)
    line.append(10)
    try FileHandle.standardOutput.write(contentsOf: line)
    for (_, bytes) in images {
        try FileHandle.standardOutput.write(contentsOf: bytes)
    }
}

func elapsedMs(since start: UInt64) -> Double {
    Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000
}

func captureConfiguration(width: Double, height: Double) -> SCStreamConfiguration {
    let configuration = SCStreamConfiguration()
    let scale = min(1, 1920 / max(width, height))
    configuration.width = max(1, Int(width * scale))
    configuration.height = max(1, Int(height * scale))
    configuration.showsCursor = false
    return configuration
}

@main
struct VisualSensor {
    static func main() async {
        umask(0o077)
        do {
            try await run(Array(CommandLine.arguments.dropFirst()))
        } catch {
            FileHandle.standardError.write(Data("visual-sensor: \(error.localizedDescription)\n".utf8))
            exit(1)
        }
    }

    @MainActor
    private static func run(_ args: [String]) async throws {
        switch args.first {
        case "capture" where (3...4).contains(args.count),
             "capture-window" where (4...5).contains(args.count),
             "watch-window" where args.count >= 5:
            let watching = args[0] == "watch-window"
            guard let count = watching ? 0 : Int(args[2]), count >= 0 else { throw SensorError("Invalid frame count.") }
            let windowBundle = watching ? args[2] : args[0] == "capture-window" ? args[3] : nil
            let archiveIndex = windowBundle == nil ? 3 : 4
            let profile = try loadProfile(args[1])
            let whole = watching && args[3] == "whole"
            let watched = watching ? Set(args.dropFirst(4)) : []
            if watching {
                guard (whole || args[3] == "crops"), watched.isSubset(of: Set(profile.fields.map(\.id))),
                      fcntl(STDIN_FILENO, F_SETFL, O_NONBLOCK) != -1 else {
                    throw SensorError("Invalid watcher fields, image mode, or command input.")
                }
            }
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            let filter: SCContentFilter
            let configuration: SCStreamConfiguration
            if let windowBundle {
                let windows = content.windows.filter {
                    $0.owningApplication?.bundleIdentifier == windowBundle &&
                        $0.windowLayer == 0 && $0.frame.width > 0 && $0.frame.height > 0
                }
                guard let window = windows.max(by: {
                    $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height
                }) else {
                    throw SensorError("No on-screen window matched the application bundle ID.")
                }
                // The standalone CLI needs a WindowServer connection before creating a window filter.
                _ = NSApplication.shared
                filter = SCContentFilter(desktopIndependentWindow: window)
                configuration = captureConfiguration(width: window.frame.width * Double(filter.pointPixelScale),
                                                     height: window.frame.height * Double(filter.pointPixelScale))
                configuration.ignoreShadowsSingleWindow = true
            } else {
                guard let display = content.displays.first(where: { $0.displayID == CGMainDisplayID() }) else {
                    throw SensorError("The primary display is unavailable.")
                }
                filter = SCContentFilter(display: display, excludingWindows: [])
                configuration = captureConfiguration(width: Double(display.width), height: Double(display.height))
            }
            var sensor = try Sensor(profile: profile,
                                    archivePath: !watching && args.count > archiveIndex ? args[archiveIndex] : nil)
            var frame = 1
            while count == 0 || frame <= count {
                let start = DispatchTime.now().uptimeNanoseconds
                var command: UInt8 = 0
                let fresh = watching && Darwin.read(STDIN_FILENO, &command, 1) == 1
                let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
                let captureMs = elapsedMs(since: start)
                let capturedAt = Date()
                if watching {
                    let observation = try sensor.observe(image, frame: frame, capturedAt: capturedAt, captureMs: captureMs)
                    try writeWatchSample(observation, image: image, profile: profile, watched: watched,
                                         whole: whole, fresh: fresh, encoder: sensor.encoder)
                } else {
                    try sensor.accept(image, frame: frame, capturedAt: capturedAt, captureMs: captureMs)
                }
                frame += 1
                if count == 0 || frame <= count {
                    let remainingMs = Double(sensor.profile.intervalMs) - elapsedMs(since: start)
                    if remainingMs > 0 {
                        try await Task.sleep(for: .seconds(remainingMs / 1000))
                    }
                }
            }
        case "replay" where args.count >= 3:
            var sensor = try Sensor(profile: loadProfile(args[1]), archivePath: nil)
            for (index, path) in args.dropFirst(2).enumerated() {
                let start = DispatchTime.now().uptimeNanoseconds
                let image = try readImage(URL(fileURLWithPath: path))
                guard image.width <= 1920, image.height <= 1920 else {
                    throw SensorError("Replay image exceeds 1920 pixels on an axis.")
                }
                try sensor.accept(image, frame: index + 1, capturedAt: Date(), captureMs: elapsedMs(since: start))
            }
        case "inspect" where args.count == 5 || args.count == 7:
            guard let session = UUID(uuidString: args[2]), let frame = Int(args[3]), frame > 0 else {
                throw SensorError("Invalid session or frame ID.")
            }
            let source = URL(fileURLWithPath: args[1])
                .appendingPathComponent(session.uuidString.lowercased()).appendingPathComponent("\(frame).png")
            let image = try readImage(source)
            let output = URL(fileURLWithPath: args[4])
            if args.count == 7 {
                let profile = try loadProfile(args[5])
                guard let field = profile.fields.first(where: { $0.id == args[6] }),
                      let crop = image.cropping(to: field.region.rect(in: image)) else {
                    throw SensorError("Field is unavailable in the supplied profile.")
                }
                try writePNG(crop, to: output)
            } else {
                try FileManager.default.copyItem(at: source, to: output)
            }
        default:
            throw SensorError("""
                Usage:
                  visual-sensor capture PROFILE FRAME_COUNT [ARCHIVE_ROOT]
                  visual-sensor capture-window PROFILE FRAME_COUNT BUNDLE_ID [ARCHIVE_ROOT]
                  visual-sensor watch-window PROFILE BUNDLE_ID crops|whole FIELD [FIELD ...]
                  visual-sensor replay PROFILE IMAGE [IMAGE ...]
                  visual-sensor inspect ARCHIVE_ROOT SESSION FRAME OUTPUT.png [PROFILE FIELD]
                A frame count of 0 runs until stopped.
                """)
        }
    }
}
