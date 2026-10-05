import SwiftUI
import QuickLook
import UIKit
import ImageIO

struct ArtifactCardView: SwiftUI.View {
    var file: ArtifactFile
    var caption: String? = nil
    var download: ((ArtifactFile) async throws -> Data)?
    @State private var loading = false
    @State private var failure: String?
    @State private var localURL: URL?
    @State private var previewURL: URL?
    @State private var thumbnail: UIImage?
    @State private var activeContent = false

    var body: some SwiftUI.View {
        VStack(alignment: .leading, spacing: 12) {
            if let thumbnail {
                Button { previewURL = localURL } label: {
                    Image(uiImage: thumbnail)
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: .infinity, maxHeight: 160)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Open " + file.name)
            }
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "doc")
                    .font(.title2)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(file.name)
                        .font(.subheadline.weight(.semibold))
                        .fixedSize(horizontal: false, vertical: true)
                    Text(ByteCountFormatter.string(fromByteCount: Int64(clamping: file.size), countStyle: .file))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            if let caption = caption, !caption.isEmpty, caption != file.name {
                Text(caption)
                    .font(.subheadline)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let sha = file.gitSha {
                Text("Git SHA: " + sha)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                Button("Copy Git SHA") { UIPasteboard.general.string = sha }
                    .accessibilityIdentifier("artifact-copy-sha")
            }
            if let localURL {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 12) { fileActions(localURL) }
                    VStack(alignment: .leading, spacing: 8) { fileActions(localURL) }
                }
            } else if download != nil {
                Button { Swift.Task { await fetch() } } label: {
                    HStack(spacing: 8) {
                        if loading { ProgressView().controlSize(.small) }
                        Label(loading ? "Downloading…" : "Download", systemImage: "arrow.down.circle")
                    }
                    .frame(minHeight: 32)
                }
                .disabled(loading)
                .accessibilityIdentifier("artifact-download")
            }
            if let failure {
                Text(failure)
                    .font(.caption)
                    .foregroundStyle(Ink.bad)
                    .accessibilityIdentifier("artifact-error")
            }
        }
        .buttonStyle(.bordered)
        .confirmationDialog("Open active content in another app?", isPresented: $activeContent) {
            if let localURL { ShareLink("Open in another app", item: localURL) }
        }
        .quickLookPreview($previewURL)
        .task(id: file.id) {
            if rasterImage && file.size <= 5 * 1024 * 1024 && localURL == nil {
                await fetch()
            }
        }
    }

    @ViewBuilder
    private func fileActions(_ url: URL) -> some SwiftUI.View {
        Button("Open", systemImage: "doc.text.magnifyingglass") {
            if file.mediaType == "text/html" || file.mediaType == "image/svg+xml" {
                activeContent = true
            } else {
                previewURL = url
            }
        }
        .accessibilityIdentifier("artifact-open")
        ShareLink(item: url) { Label("Save", systemImage: "square.and.arrow.up") }
            .accessibilityIdentifier("artifact-save")
    }

    private var rasterImage: Bool {
        ["image/png", "image/jpeg", "image/webp", "image/gif"].contains(file.mediaType)
    }

    private func fetch() async {
        guard let download, !loading else { return }
        loading = true
        failure = nil
        defer { loading = false }
        do {
            let bytes = try await download(file)
            let directory = FileManager.default.temporaryDirectory
                .appendingPathComponent("kyoto-artifacts", isDirectory: true)
                .appendingPathComponent(UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            let destination = directory.appendingPathComponent(file.name)
            try bytes.write(to: destination, options: [.atomic, .completeFileProtection])
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: destination.path)
            localURL = destination
            if rasterImage, let source = CGImageSourceCreateWithURL(destination as CFURL, nil),
               let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceThumbnailMaxPixelSize: 480,
                kCGImageSourceCreateThumbnailWithTransform: true
               ] as CFDictionary) {
                thumbnail = UIImage(cgImage: image)
            }
        } catch is CancellationError {
        } catch {
            failure = (error as? HostError)?.serverMessage ?? error.localizedDescription
        }
    }
}
