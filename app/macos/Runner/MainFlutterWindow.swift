import Cocoa
import Foundation
import FlutterMacOS

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)
    registerClipboardChannel(flutterViewController: flutterViewController)

    super.awakeFromNib()
  }

  private func registerClipboardChannel(flutterViewController: FlutterViewController) {
    let channel = FlutterMethodChannel(
      name: "com.clipper.app/secure_clipboard",
      binaryMessenger: flutterViewController.engine.binaryMessenger)

    channel.setMethodCallHandler { call, result in
      switch call.method {
      case "setText":
        guard let args = call.arguments as? [String: Any],
              let text = args["text"] as? String else {
          result(FlutterError(code: "invalid_args", message: "Missing clipboard text", details: nil))
          return
        }
        self.setClipboardText(text)
        result(nil)
      case "setEntry":
        guard let args = call.arguments as? [String: Any],
              let mimeType = args["mimeType"] as? String,
              let bytes = args["bytes"] as? FlutterStandardTypedData else {
          result(FlutterError(code: "invalid_args", message: "Missing clipboard payload", details: nil))
          return
        }
        let text = args["text"] as? String
        do {
          try self.setClipboardEntry(mimeType: mimeType, data: bytes.data, text: text)
          result(nil)
        } catch {
          result(FlutterError(code: "clipboard_write_failed", message: error.localizedDescription, details: nil))
        }
      case "getEntry":
        result(self.readClipboardEntry())
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  private func setClipboardText(_ text: String) {
    let pasteboard = NSPasteboard.general
    pasteboard.clearContents()
    pasteboard.setString(text, forType: .string)
  }

  private func setClipboardEntry(mimeType: String, data: Data, text: String?) throws {
    if mimeType.hasPrefix("text/") {
      setClipboardText(text ?? String(data: data, encoding: .utf8) ?? "")
      return
    }

    if let pasteboardType = pasteboardTypeForImageMimeType(mimeType) {
      let pasteboard = NSPasteboard.general
      pasteboard.clearContents()
      pasteboard.setData(data, forType: pasteboardType)
      return
    }

    throw ClipboardChannelError.unsupportedMimeType(mimeType)
  }

  private func readClipboardEntry() -> [String: Any]? {
    let pasteboard = NSPasteboard.general
    if let data = pasteboard.data(forType: .png), !data.isEmpty {
      return [
        "mimeType": "image/png",
        "bytes": FlutterStandardTypedData(bytes: data)
      ]
    }

    guard let text = pasteboard.string(forType: .string), !text.isEmpty else {
      return nil
    }
    return [
      "mimeType": "text/plain",
      "bytes": FlutterStandardTypedData(bytes: Data(text.utf8)),
      "text": text
    ]
  }

  private func pasteboardTypeForImageMimeType(_ mimeType: String) -> NSPasteboard.PasteboardType? {
    switch mimeType.lowercased() {
    case "image/png":
      return .png
    case "image/jpeg", "image/jpg":
      return NSPasteboard.PasteboardType("public.jpeg")
    case "image/gif":
      return NSPasteboard.PasteboardType("com.compuserve.gif")
    case "image/webp":
      return NSPasteboard.PasteboardType("org.webmproject.webp")
    default:
      return nil
    }
  }
}

private enum ClipboardChannelError: LocalizedError {
  case unsupportedMimeType(String)

  var errorDescription: String? {
    switch self {
    case .unsupportedMimeType(let mimeType):
      return "Unsupported clipboard MIME type: \(mimeType)"
    }
  }
}
