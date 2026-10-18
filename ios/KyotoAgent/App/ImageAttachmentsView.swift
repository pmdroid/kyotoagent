import ImageIO
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import UIKit

struct ImageAttachmentsView: SwiftUI.View {
    @Bindable var model: AppModel
    @State private var picks: [PhotosPickerItem] = []

    var body: some SwiftUI.View {
        PhotosPicker(selection: $picks, maxSelectionCount: 4, matching: .images) {
            Label("Attach images", systemImage: "photo.on.rectangle")
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.glass)
        .buttonBorderShape(.circle)
        .controlSize(.regular)
        .frame(minWidth: 44, minHeight: 44)
        .foregroundStyle(Ink.accent)
        .disabled(model.sending || model.images.count >= 4)
        .accessibilityIdentifier("composer-images")
        .onChange(of: picks) { _, selected in
            let sessionID = model.selection
            Swift.Task {
                for item in selected {
                    do {
                        if let data = try await item.loadTransferable(type: Data.self) {
                            attachImage(data, model: model, sessionID: sessionID)
                        }
                    } catch {
                        model.imageFailure(error.localizedDescription)
                    }
                }
                picks = []
            }
        }
    }
}

struct ChatImageDrop: ViewModifier {
    var model: AppModel
    @State private var targeted = false

    func body(content: Content) -> some SwiftUI.View {
        content
            .overlay {
                if targeted {
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(Ink.accent, lineWidth: 2)
                        .allowsHitTesting(false)
                }
            }
            .onDrop(of: [UTType.image], isTargeted: $targeted) { providers in
                let sessionID = model.selection
                guard sessionID != nil, !model.sending else { return false }
                let images = providers.compactMap { provider -> (NSItemProvider, String)? in
                    guard let type = provider.registeredTypeIdentifiers.first(where: {
                        UTType($0)?.conforms(to: .image) == true
                    }) else { return nil }
                    return (provider, type)
                }
                for (provider, type) in images {
                    provider.loadDataRepresentation(forTypeIdentifier: type) { data, error in
                        Swift.Task { @MainActor in
                            if let data {
                                attachImage(data, model: model, sessionID: sessionID)
                            } else {
                                model.imageFailure(error?.localizedDescription ?? "The image could not be opened.")
                            }
                        }
                    }
                }
                return !images.isEmpty
            }
    }
}

@MainActor
private func attachImage(_ data: Data, model: AppModel, sessionID: String?) {
    guard data.count <= 20 * 1024 * 1024,
          let source = CGImageSourceCreateWithData(data as CFData, nil),
          let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 4096,
          ] as CFDictionary),
          let bytes = UIImage(cgImage: thumbnail).jpegData(compressionQuality: 0.85),
          bytes.count <= 5 * 1024 * 1024 else {
        model.imageFailure("Choose an image that fits within 5 MiB.")
        return
    }
    model.addImage(ImageAttachment(name: "photo-\(UUID().uuidString).jpg", mimeType: "image/jpeg", data: bytes.base64EncodedString()), sessionID: sessionID)
}

struct ImageAttachmentStrip: SwiftUI.View {
    var images: [ImageAttachment]
    var onOpen: (ImageAttachment) -> Void
    var onRemove: ((String) -> Void)? = nil

    var body: some SwiftUI.View {
        if !images.isEmpty {
            ScrollView(.horizontal) {
                HStack(spacing: 10) {
                    ForEach(images) { image in
                        VStack(spacing: 4) {
                            ImageAttachmentButton(attachment: image, onOpen: onOpen)
                            if let onRemove {
                                Button("Remove image", systemImage: "xmark.circle.fill") {
                                    onRemove(image.id)
                                }
                                .labelStyle(.iconOnly)
                                .foregroundStyle(Ink.faint)
                                .accessibilityLabel("Remove " + image.name)
                            }
                        }
                    }
                }
            }
        }
    }
}

struct ImageAttachmentButton: SwiftUI.View {
    var attachment: ImageAttachment
    var onOpen: (ImageAttachment) -> Void

    var body: some SwiftUI.View {
        if let bytes = attachment.bytes, let image = UIImage(data: bytes) {
            Button {
                onOpen(attachment)
            } label: {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
                    .frame(width: 72, height: 72)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Open " + attachment.name)
        }
    }
}

struct ImagePreviewView: SwiftUI.View {
    var attachment: ImageAttachment

    var body: some SwiftUI.View {
        if let bytes = attachment.bytes, let image = UIImage(data: bytes) {
            Image(uiImage: image)
                .resizable()
                .scaledToFit()
                .padding(16)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityLabel(attachment.name)
                .accessibilityIdentifier("image-preview")
        }
    }
}
