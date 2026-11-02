import Foundation

#if canImport(FoundationModels)
import FoundationModels

/// Swift concurrency cannot capture a mutable `var` across a `Task`, so the
/// result travels home in a box.
private final class ResultBox: @unchecked Sendable {
    var value: String?
}

/// Blocking wrapper used by the Rust worker thread. The worker is already off
/// the UI thread, so waiting here is fine.
@available(macOS 27.0, *)
private func generateCommand(_ prompt: String) -> String? {
    let box = ResultBox()
    let semaphore = DispatchSemaphore(value: 0)
    Task.detached {
        do {
            let session = LanguageModelSession()
            let response = try await session.respond(to: prompt)
            box.value = response.content
        } catch {
            box.value = nil
        }
        semaphore.signal()
    }
    semaphore.wait()
    return box.value
}
#endif

@_cdecl("prompt_fm_available")
public func prompt_fm_available() -> Int32 {
    #if canImport(FoundationModels)
    if #available(macOS 27.0, *) {
        if case .available = SystemLanguageModel.default.availability {
            return 1
        }
    }
    #endif
    return 0
}

@_cdecl("prompt_fm_generate")
public func prompt_fm_generate(_ prompt: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    guard let prompt else { return nil }
    let input = String(cString: prompt)
    #if canImport(FoundationModels)
    if #available(macOS 27.0, *) {
        if let output = generateCommand(input) {
            return strdup(output)
        }
    }
    #endif
    return nil
}

@_cdecl("prompt_fm_free")
public func prompt_fm_free(_ pointer: UnsafeMutablePointer<CChar>?) {
    free(pointer)
}
