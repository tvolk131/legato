import SwiftUI

/// Drag paired devices to where they sit on the desk. Drops snap flush against this
/// Mac's displays (in Rust, shared with the CLI and Windows app); touching edges are
/// where the pointer crosses.
struct ArrangementView: View {
    var model: AppModel
    @State private var dragging: String?
    @State private var translation = CGSize.zero

    private var arrangement: Arrangement { model.state.arrangement }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Drag devices to match your desk. Touching screen edges let the pointer move between devices.")
                .foregroundStyle(.secondary).padding(.horizontal, 24)
            GeometryReader { geometry in
                let all = arrangement.local + arrangement.peers.flatMap(\.displays)
                let bounds = all.reduce(CGRect.null) { $0.union($1.cg) }
                let scale = bounds.isNull ? 1 : min(
                    (geometry.size.width - 64) / max(bounds.width, 1),
                    (geometry.size.height - 64) / max(bounds.height, 1))
                let origin = CGPoint(
                    x: (geometry.size.width - bounds.width * scale) / 2 - bounds.minX * scale,
                    y: (geometry.size.height - bounds.height * scale) / 2 - bounds.minY * scale)
                ZStack(alignment: .topLeading) {
                    Color(nsColor: .controlBackgroundColor)
                    ForEach(Array(arrangement.local.enumerated()), id: \.offset) { index, rect in
                        screen(rect, name: label(model.state.name, index, primary: arrangement.localPrimary), local: true, scale: scale)
                            .position(center(rect, scale: scale, origin: origin))
                    }
                    ForEach(arrangement.peers) { peer in
                        ForEach(Array(peer.displays.enumerated()), id: \.offset) { index, rect in
                            screen(rect, name: label(peer.name, index, primary: peer.primary) + (peer.placed ? "" : " · Unplaced"), local: false, scale: scale)
                                .offset(dragging == peer.id ? translation : .zero)
                                .gesture(drag(peer, scale: scale))
                                .position(center(rect, scale: scale, origin: origin))
                                .accessibilityHint("Drag to arrange, or use the Position menu below.")
                        }
                    }
                    if all.isEmpty {
                        ContentUnavailableView("No displays available", systemImage: "display")
                            .frame(width: geometry.size.width, height: geometry.size.height)
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 12))
            }
            .frame(minHeight: 240).padding(.horizontal, 24)
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(arrangement.problems, id: \.self) {
                        Text($0).font(.callout).foregroundStyle(.secondary)
                    }
                    ForEach(arrangement.peers) { peer in
                        HStack {
                            Text(peer.name).lineLimit(2)
                            if !peer.placed { Text("Choose a position").foregroundStyle(.secondary) }
                            Spacer()
                            positionMenu(peer)
                        }
                        .padding(.vertical, 4)
                    }
                }
                .padding(.horizontal, 24)
            }
            .frame(maxHeight: 120)
        }
        .padding(.vertical, 20)
    }

    private func label(_ name: String, _ index: Int, primary: Int?) -> String {
        "\(name) · \(index + 1)" + (primary == index ? " ★" : "")
    }

    private func center(_ rect: DeskRect, scale: Double, origin: CGPoint) -> CGPoint {
        CGPoint(x: origin.x + (rect.x + rect.width / 2) * scale, y: origin.y + (rect.y + rect.height / 2) * scale)
    }

    private func drag(_ peer: ArrangedPeer, scale: Double) -> some Gesture {
        DragGesture()
            .onChanged { value in
                dragging = peer.id
                translation = value.translation
            }
            .onEnded { value in
                let box = peer.bounds
                place(peer, x: box.minX + value.translation.width / scale, y: box.minY + value.translation.height / scale)
                dragging = nil
                translation = .zero
            }
    }

    private func screen(_ rect: DeskRect, name: String, local: Bool, scale: Double) -> some View {
        let tint = local ? Color.secondary : Color.accentColor
        return RoundedRectangle(cornerRadius: 6).fill(tint.opacity(local ? 0.15 : 0.2))
            .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(tint, lineWidth: 2) }
            .overlay { Text(name).font(.caption).multilineTextAlignment(.center).padding(4) }
            .frame(width: max(rect.width * scale, 30), height: max(rect.height * scale, 24))
            .contentShape(Rectangle())
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(name)
    }

    /// Keyboard and VoiceOver access: put a device beside one of this Mac's displays.
    private func positionMenu(_ peer: ArrangedPeer) -> some View {
        Menu("Position") {
            ForEach(Array(arrangement.local.enumerated()), id: \.offset) { index, local in
                Section("Display \(index + 1)") {
                    ForEach(Side.allCases) { side in
                        Button(side.rawValue) { place(peer, nextTo: local, side: side) }
                    }
                }
            }
        }
        .fixedSize()
        .accessibilityLabel("Position of \(peer.name)")
    }

    private enum Side: String, CaseIterable, Identifiable {
        case above = "Above", below = "Below", left = "Left", right = "Right"
        var id: Self { self }
    }

    private func place(_ peer: ArrangedPeer, nextTo local: DeskRect, side: Side) {
        let box = peer.bounds
        var x = local.x + (local.width - box.width) / 2
        var y = local.y + (local.height - box.height) / 2
        switch side {
        case .above: y = local.y - box.height
        case .below: y = local.y + local.height
        case .left: x = local.x - box.width
        case .right: x = local.x + local.width
        }
        place(peer, x: x, y: y)
    }

    private func place(_ peer: ArrangedPeer, x: Double, y: Double) {
        model.command("place", ["id": peer.id, "x": x, "y": y])
    }
}
