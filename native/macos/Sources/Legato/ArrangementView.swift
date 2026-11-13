import SwiftUI

struct ArrangementView: View {
    var model: AppModel
    @State private var dragging: String?
    @State private var translation = CGSize.zero
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Drag devices to match your desk. Touching screen edges let the pointer move between devices.")
                .foregroundStyle(.secondary).padding(.horizontal, 24)
            GeometryReader { geometry in
                let arrangement = model.state.arrangement
                let all = arrangement.local + arrangement.peers.flatMap(\.displays)
                let bounds = all.reduce(CGRect.null) { $0.union($1.cg) }
                let scale = bounds.isNull ? 1 : min((geometry.size.width - 64) / max(bounds.width, 1), (geometry.size.height - 64) / max(bounds.height, 1))
                let origin = CGPoint(x: (geometry.size.width - bounds.width * scale) / 2 - bounds.minX * scale, y: (geometry.size.height - bounds.height * scale) / 2 - bounds.minY * scale)
                ZStack(alignment: .topLeading) {
                    Color(nsColor: .controlBackgroundColor)
                    ForEach(Array(arrangement.local.enumerated()), id: \.offset) { index, rect in
                        screen(rect, name: "\(model.state.name) · \(index + 1)\(arrangement.local_primary?.contains(index) == true ? " ★" : "")", local: true, scale: scale, origin: origin)
                            .position(x: origin.x + (rect.x + rect.width / 2) * scale, y: origin.y + (rect.y + rect.height / 2) * scale)
                    }
                    ForEach(arrangement.peers) { peer in
                        ForEach(Array(peer.displays.enumerated()), id: \.offset) { index, rect in
                            screen(rect, name: "\(peer.name) · \(index + 1)\(peer.placed ? "" : " · Unplaced")", local: false, scale: scale, origin: origin)
                                .offset(dragging == peer.id ? translation : .zero)
                                .gesture(DragGesture(coordinateSpace: .named("desk")).onChanged { value in dragging = peer.id; translation = value.translation }
                                    .onEnded { value in
                                        let box = peer.displays.reduce(CGRect.null) { $0.union($1.cg) }
                                        model.command("place", ["id": peer.id, "x": box.minX + value.translation.width / scale, "y": box.minY + value.translation.height / scale])
                                        dragging = nil; translation = .zero
                                    })
                                .position(x: origin.x + (rect.x + rect.width / 2) * scale, y: origin.y + (rect.y + rect.height / 2) * scale)
                                .accessibilityHint("Drag to arrange. Use the placement menu for keyboard access.")
                        }
                    }
                    if all.isEmpty { ContentUnavailableView("No displays available", systemImage: "display").frame(width: geometry.size.width, height: geometry.size.height) }
                }.coordinateSpace(name: "desk").clipShape(RoundedRectangle(cornerRadius: 12))
            }.frame(minHeight: 240).padding(.horizontal, 24)
            ScrollView {
                ForEach(model.state.arrangement.problems, id: \.self) { Text($0).font(.callout).foregroundStyle(.secondary).padding(.horizontal, 24) }
                ForEach(model.state.arrangement.peers) { peer in
                    HStack {
                        Text(peer.name).lineLimit(2)
                        if !peer.placed { Text("Choose a position").foregroundStyle(.secondary) }
                        Spacer()
                        Menu("Position") {
                            ForEach(Array(model.state.arrangement.local.enumerated()), id: \.offset) { index, local in
                                Menu("Display \(index + 1)") {
                                    ForEach(["Above", "Below", "Left", "Right"], id: \.self) { side in
                                        Button(side) { place(peer, nextTo: local, side: side) }
                                    }
                                }
                            }
                        }.fixedSize()
                    }.padding(.horizontal, 24).padding(.vertical, 4)
                }
            }.frame(maxHeight: 100)
        }.padding(.vertical, 20)
    }
    func screen(_ rect: DeskRect, name: String, local: Bool, scale: Double, origin: CGPoint) -> some View {
        RoundedRectangle(cornerRadius: 6).fill(local ? Color.secondary.opacity(0.15) : Color.accentColor.opacity(0.2))
            .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(local ? Color.secondary : Color.accentColor, lineWidth: 2) }
            .overlay { Text(name).font(.caption).multilineTextAlignment(.center).padding(4) }
            .frame(width: max(rect.width * scale, 30), height: max(rect.height * scale, 24))
            .contentShape(Rectangle())
            .accessibilityLabel(name)
    }
    func place(_ peer: DeskPeer, nextTo local: DeskRect, side: String) {
        let box = peer.displays.reduce(CGRect.null) { $0.union($1.cg) }
        var x = local.x + (local.width - box.width) / 2
        var y = local.y + (local.height - box.height) / 2
        switch side {
        case "Above": y = local.y - box.height
        case "Below": y = local.y + local.height
        case "Left": x = local.x - box.width
        default: x = local.x + local.width
        }
        model.command("place", ["id": peer.id, "x": x, "y": y])
    }
}
