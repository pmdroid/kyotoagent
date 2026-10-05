import SwiftUI

struct PhonePopup<Content: SwiftUI.View>: SwiftUI.View {
    var onClose: () -> Void
    @ViewBuilder var content: () -> Content

    var body: some SwiftUI.View {
        content()
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .onKeyPress(.escape) {
                onClose()
                return .handled
            }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .accessibilityIdentifier("phone-popup")
    }
}

extension SwiftUI.View {
    func phonePopup<Popup: SwiftUI.View>(
        isPresented: Binding<Bool>,
        @ViewBuilder content: @escaping () -> Popup
    ) -> some SwiftUI.View {
        sheet(isPresented: isPresented) {
            PhonePopup(onClose: { isPresented.wrappedValue = false }, content: content)
        }
    }

    func phonePopup<Item: Identifiable, Popup: SwiftUI.View>(
        item: Binding<Item?>,
        @ViewBuilder content: @escaping (Item) -> Popup
    ) -> some SwiftUI.View {
        sheet(item: item) { value in
            PhonePopup(onClose: { item.wrappedValue = nil }) {
                content(value)
            }
        }
    }
}
