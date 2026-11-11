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

struct Reading {
    let value: String
    let confidence: Float
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

    init(_ config: FieldConfig) throws {
        guard config.region.isValid, !config.id.isEmpty, config.id.utf8.count <= 64,
              (1...20).contains(config.confirmations ?? 1),
              (config.pattern?.utf8.count ?? 0) <= 512 else {
            throw SensorError("Invalid field ID, region, pattern size, or confirmation count.")
        }
        self.config = config
        pattern = try config.pattern.map { try NSRegularExpression(pattern: $0) }
    }

    mutating func process(_ image: CGImage, frame: Int) throws -> (FieldState, Change?) {
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
        if let reading = lastReading {
            if reading.value == pending {
                consecutive = min(consecutive + 1, config.confirmations ?? 1)
            } else {
                pending = reading.value
                consecutive = 1
            }

            if consecutive >= (config.confirmations ?? 1) {
                if reading.value != value {
                    change = Change(field: config.id, previous: value, value: reading.value)
                }
                value = reading.value
                valueFrame = frame
            }
        } else {
            pending = nil
            consecutive = 0
        }

        let status: String
        if lastReading == nil {
            status = "unreadable"
        } else if pending != value {
            status = "confirming"
        } else {
            status = "observed"
        }

        return (FieldState(id: config.id, status: status, value: value,
                           candidate: status == "confirming" ? pending : nil, valueFrame: valueFrame,
                           confidence: lastReading?.confidence, ocrReused: reused), change)
    }

    private func recognize(_ crop: CGImage) throws -> Reading? {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .fast
        request.usesLanguageCorrection = false
        try VNImageRequestHandler(cgImage: crop).perform([request])
        let lines = (request.results ?? []).prefix(64).compactMap { $0.topCandidates(1).first }
        let text = String(lines.map(\.string).joined(separator: "\n").prefix(2048))
        guard !text.isEmpty, let confidence = lines.map(\.confidence).min() else { return nil }

        guard let pattern else { return Reading(value: text, confidence: confidence) }
        guard let match = pattern.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) else {
            return nil
        }
        let matchRange = match.range(at: match.numberOfRanges > 1 ? 1 : 0)
        guard let range = Range(matchRange, in: text), !range.isEmpty else { return nil }
        return Reading(value: String(text[range]), confidence: confidence)
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

    mutating func accept(_ image: CGImage, frame: Int, capturedAt: Date, captureMs: Double) throws {
        let start = DispatchTime.now().uptimeNanoseconds
        var states: [FieldState] = []
        var changes: [Change] = []
        for index in fields.indices {
            let (state, change) = try fields[index].process(image, frame: frame)
            states.append(state)
            if let change { changes.append(change) }
        }
        let processingMs = elapsedMs(since: start)
        try archive?.store(image, frame: frame)
        let observation = Observation(session: session, profile: profile.id, frame: frame,
                                      capturedAtUnixMs: Int64(capturedAt.timeIntervalSince1970 * 1000),
                                      width: image.width, height: image.height,
                                      captureMs: captureMs, processingMs: processingMs,
                                      evidenceRetained: archive != nil, fields: states, changes: changes)
        var line = try encoder.encode(observation)
        line.append(10)
        try FileHandle.standardOutput.write(contentsOf: line)
    }
}

func elapsedMs(since start: UInt64) -> Double {
    Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000
}

func captureConfiguration(_ display: SCDisplay) -> SCStreamConfiguration {
    let configuration = SCStreamConfiguration()
    let scale = min(1, 1920 / Double(max(display.width, display.height)))
    configuration.width = max(1, Int(Double(display.width) * scale))
    configuration.height = max(1, Int(Double(display.height) * scale))
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

    private static func run(_ args: [String]) async throws {
        switch args.first {
        case "capture" where (3...4).contains(args.count):
            guard let count = Int(args[2]), count >= 0 else { throw SensorError("Invalid frame count.") }
            var sensor = try Sensor(profile: loadProfile(args[1]), archivePath: args.count == 4 ? args[3] : nil)
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            guard let display = content.displays.first(where: { $0.displayID == CGMainDisplayID() }) else {
                throw SensorError("The primary display is unavailable.")
            }
            let filter = SCContentFilter(display: display, excludingWindows: [])
            let configuration = captureConfiguration(display)
            var frame = 1
            while count == 0 || frame <= count {
                let start = DispatchTime.now().uptimeNanoseconds
                let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
                try sensor.accept(image, frame: frame, capturedAt: Date(), captureMs: elapsedMs(since: start))
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
                  visual-sensor replay PROFILE IMAGE [IMAGE ...]
                  visual-sensor inspect ARCHIVE_ROOT SESSION FRAME OUTPUT.png [PROFILE FIELD]
                A frame count of 0 runs until stopped.
                """)
        }
    }
}
