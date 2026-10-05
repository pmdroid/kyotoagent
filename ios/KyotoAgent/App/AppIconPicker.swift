import SwiftUI
import UIKit

struct AppIconPicker: SwiftUI.View {
    @State private var selected = UIApplication.shared.alternateIconName
    @State private var changing = false
    @State private var failure: String?

    var body: some SwiftUI.View {
        List {
            Section {
                icon("Kyoto", preview: "AppIconPreview", name: nil)
                icon("Mochi", preview: "KyotoIcon2Preview", name: "KyotoIcon2")
                icon("Sora", preview: "KyotoIcon3Preview", name: "KyotoIcon3")
                icon("Yuki", preview: "KyotoIcon4Preview", name: "KyotoIcon4")
                icon("Haru", preview: "KyotoIcon5Preview", name: "KyotoIcon5")
                icon("Momo", preview: "KyotoIcon6Preview", name: "KyotoIcon6")
                icon("Kumo", preview: "KyotoIcon7Preview", name: "KyotoIcon7")
                icon("Rin", preview: "KyotoIcon8Preview", name: "KyotoIcon8")
            }
            if changing {
                ProgressView("Changing icon…")
            }
            if let failure {
                Text(failure).foregroundStyle(.red)
            }
            if !UIApplication.shared.supportsAlternateIcons {
                Text("App icon changes are unavailable on this device.")
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("App icon")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { selected = UIApplication.shared.alternateIconName }
    }

    private func icon(_ title: String, preview: String, name: String?) -> some SwiftUI.View {
        Button {
            Swift.Task { await change(to: name) }
        } label: {
            HStack(spacing: 16) {
                previewImage(preview)
                    .resizable()
                    .frame(width: 60, height: 60)
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                    .accessibilityHidden(true)
                Text(title)
                    .foregroundStyle(.primary)
                Spacer(minLength: 8)
                if selected == name {
                    Image(systemName: "checkmark")
                        .foregroundStyle(.tint)
                }
            }
            .padding(.vertical, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(changing || !UIApplication.shared.supportsAlternateIcons)
        .accessibilityAddTraits(selected == name ? .isSelected : [])
        .accessibilityIdentifier(name ?? "PrimaryIcon")
    }

    private func previewImage(_ name: String) -> Image {
        guard let url = Bundle.main.url(forResource: name, withExtension: "png"),
              let image = UIImage(contentsOfFile: url.path) else {
            return Image(systemName: "app")
        }
        return Image(uiImage: image)
    }

    private func change(to name: String?) async {
        guard !changing, selected != name else { return }
        changing = true
        failure = nil
        defer {
            selected = UIApplication.shared.alternateIconName
            changing = false
        }
        do {
            try await UIApplication.shared.setAlternateIconName(name)
        } catch {
            failure = error.localizedDescription
        }
    }
}
