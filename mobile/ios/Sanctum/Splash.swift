import SwiftUI

/// Launch animation over the first moments while chats load: the mark draws
/// itself in, fills, and then the app blooms open out of its centre dot.
/// Lasts at least ~2.4 s and never finishes before `ready`; Reduce Motion
/// gets a plain fade.
struct SplashView: View {
    /// Chats are loaded; the bloom waits for this.
    let ready: Bool
    let finished: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var draw: CGFloat = 0
    @State private var fill: Double = 0
    @State private var dotScale: CGFloat = 0
    @State private var bloom: CGFloat = 0
    @State private var settled = false
    @State private var blooming = false
    /// Never hold the app hostage: give up waiting after a few seconds.
    @State private var waitedEnough = false
    /// `ready`, mirrored into state: the async animation task holds a copy of
    /// this view, so it would only ever see the launch-time value.
    @State private var isReady = false

    var body: some View {
        GeometryReader { g in
            let side = min(g.size.width, g.size.height) * 0.44
            let frame = CGRect(
                x: (g.size.width - side) / 2,
                y: (g.size.height - side) / 2 - side * 0.1,
                width: side,
                height: side
            )
            let dot = CGPoint(x: frame.minX + Logo.dot.x * side, y: frame.minY + Logo.dot.y * side)
            let dotR = Logo.dotRadius * side * dotScale
            // Far enough from the dot to clear every corner of the screen.
            let farR = hypot(max(dot.x, g.size.width - dot.x), max(dot.y, g.size.height - dot.y))
            let hole = blooming && !reduceMotion ? dotR + (farR - dotR) * bloom : 0

            ZStack {
                Color(.systemBackground)
                let mark = Logo.outline.map { $0.applying(CGAffineTransform(scaleX: side, y: side)) }
                ZStack {
                    ForEach(mark.indices, id: \.self) { i in
                        mark[i]
                            .trim(from: 0, to: draw)
                            .stroke(Color.primary, style: StrokeStyle(lineWidth: 1.3, lineCap: .round, lineJoin: .round))
                            .opacity(1 - fill)
                    }
                    mark.reduce(into: Path()) { $0.addPath($1) }
                        .fill(Color.primary, style: FillStyle(eoFill: true))
                        .opacity(fill)
                }
                .frame(width: side, height: side, alignment: .topLeading)
                .position(x: frame.midX, y: frame.midY)
                .scaleEffect(1 + bloom * 0.18, anchor: UnitPoint(x: dot.x / g.size.width, y: dot.y / g.size.height))
                .opacity(1 - Double(bloom) * 0.6)
                Circle()
                    .fill(Color.primary)
                    .frame(width: dotR * 2, height: dotR * 2)
                    .position(dot)
            }
            // The bloom: a hole that opens from the dot and reveals the app.
            .mask {
                Rectangle()
                    .overlay {
                        Circle()
                            .frame(width: hole * 2, height: hole * 2)
                            .position(dot)
                            .blendMode(.destinationOut)
                    }
                    .compositingGroup()
            }
        }
        .ignoresSafeArea()
        .opacity(reduceMotion ? 1 - Double(bloom) : 1)
        .allowsHitTesting(!blooming)
        .task { await play() }
        .task {
            try? await Task.sleep(for: .seconds(6))
            waitedEnough = true
            maybeFinish()
        }
        .onChange(of: ready, initial: true) {
            isReady = ready
            maybeFinish()
        }
    }

    private func play() async {
        if reduceMotion {
            draw = 1; fill = 1; dotScale = 1
            try? await Task.sleep(for: .milliseconds(500))
            settled = true
            maybeFinish()
            return
        }
        withAnimation(.easeInOut(duration: 1.3)) { draw = 1 }
        try? await Task.sleep(for: .milliseconds(1050))
        withAnimation(.easeOut(duration: 0.55)) { fill = 1 }
        withAnimation(.spring(duration: 0.5, bounce: 0.35).delay(0.15)) { dotScale = 1 }
        try? await Task.sleep(for: .milliseconds(800))
        // A small swell: the dot gathers before it opens.
        withAnimation(.easeInOut(duration: 0.35)) { dotScale = 1.3 }
        try? await Task.sleep(for: .milliseconds(350))
        settled = true
        maybeFinish()
        // Still loading: breathe until ready.
        if !isReady {
            withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) { dotScale = 1.1 }
        }
    }

    private func maybeFinish() {
        guard settled, isReady || waitedEnough, !blooming else { return }
        blooming = true
        if reduceMotion {
            withAnimation(.easeOut(duration: 0.3)) { bloom = 1 } completion: { finished() }
        } else {
            // Ease in: slow to open, then a rush that fills the screen.
            withAnimation(.timingCurve(0.6, 0, 0.75, 0.25, duration: 0.75)) { bloom = 1 } completion: { finished() }
        }
    }
}
