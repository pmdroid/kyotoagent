import AVFoundation
import SwiftUI
import VisionKit

struct PairingScanner: SwiftUI.View {
    let connected: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var ready = false
    @State private var failure: String?

    var body: some SwiftUI.View {
        NavigationStack {
            Group {
                if ready {
                    QRScanner(connected: connected, failure: $failure)
                        .ignoresSafeArea(edges: .bottom)
                } else if let failure {
                    ContentUnavailableView("Camera unavailable", systemImage: "camera", description: Text(failure))
                } else {
                    ProgressView("Opening camera")
                }
            }
            .navigationTitle("Scan pairing code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                        .keyboardShortcut(.escape, modifiers: [])
                }
            }
            .task {
                guard DataScannerViewController.isSupported else {
                    failure = "Paste the connection link on this device."
                    return
                }
                guard await AVCaptureDevice.requestAccess(for: .video) else {
                    failure = "Allow camera access in Settings to scan a pairing code."
                    return
                }
                guard DataScannerViewController.isAvailable else {
                    failure = "The camera is unavailable. Try again when it is free."
                    return
                }
                ready = true
            }
            .onChange(of: failure) { _, value in
                if value != nil { ready = false }
            }
        }
    }
}

struct QRScanner: UIViewControllerRepresentable {
    let connected: (String) -> Void
    @Binding var failure: String?

    func makeCoordinator() -> Coordinator {
        Coordinator(connected: connected, failure: $failure)
    }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            recognizesMultipleItems: false,
            isHighFrameRateTrackingEnabled: false,
            isPinchToZoomEnabled: true,
            isGuidanceEnabled: true,
            isHighlightingEnabled: true
        )
        scanner.delegate = context.coordinator
        do {
            try scanner.startScanning()
        } catch {
            context.coordinator.failure.wrappedValue = "Could not open the camera. Try again."
        }
        return scanner
    }

    func updateUIViewController(_ scanner: DataScannerViewController, context: Context) {}

    static func dismantleUIViewController(_ scanner: DataScannerViewController, coordinator: Coordinator) {
        scanner.stopScanning()
    }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let connected: (String) -> Void
        let failure: Binding<String?>
        private var accepted = false

        init(connected: @escaping (String) -> Void, failure: Binding<String?>) {
            self.connected = connected
            self.failure = failure
        }

        func dataScanner(_ scanner: DataScannerViewController, didAdd addedItems: [RecognizedItem], allItems: [RecognizedItem]) {
            for item in addedItems {
                guard !accepted, case .barcode(let barcode) = item,
                    let text = barcode.payloadStringValue,
                    text.hasPrefix("kyotoagent://"), PairingConnection(text) != nil
                else { continue }
                accepted = true
                scanner.stopScanning()
                connected(text)
            }
        }

        func dataScanner(_ scanner: DataScannerViewController, becameUnavailableWithError error: DataScannerViewController.ScanningUnavailable) {
            failure.wrappedValue = "The camera is unavailable. Try scanning again."
        }
    }
}
