import CoreImage.CIFilterBuiltins
import SwiftUI

/// Bitcoin- and Zcash-style payments from the + menu, bitchat-style: you
/// paste something made in your own wallet app (a Cashu token, a Lightning
/// invoice, an address) and it arrives as a chip that opens theirs. Sanctum
/// holds no keys for these.
enum LinkPayment: String, Identifiable {
    case cashu, lightning, bitcoin, zcash
    var id: String { rawValue }

    var title: String {
        switch self {
        case .cashu: "Send Cashu Token"
        case .lightning: "Send Lightning Invoice"
        case .bitcoin: "Send Bitcoin Address"
        case .zcash: "Send Zcash Address"
        }
    }

    var placeholder: String {
        switch self {
        case .cashu: "cashuA… or cashuB…"
        case .lightning: "lnbc… invoice"
        case .bitcoin: "bc1… or bitcoin:…"
        case .zcash: "u1…, zs1…, t1… or zcash:…"
        }
    }

    var help: String {
        switch self {
        case .cashu: "Copy a token from your Cashu wallet (Send → Ecash). It's the money itself: they tap it to claim it."
        case .lightning: "Create an invoice in your Lightning wallet (Receive). They pay it from theirs."
        case .bitcoin: "Your receive address, or a bitcoin: request with an amount. They pay it from their wallet."
        case .zcash: "Your receive address, or a zcash: request with an amount and memo. They pay it from their wallet."
        }
    }

    var kind: PayKind {
        switch self {
        case .cashu: .cashu
        case .lightning: .lightning
        case .bitcoin: .bitcoin
        case .zcash: .zcash
        }
    }

    /// The text to send, so the chat turns it into a chip.
    func body(_ input: String) -> String {
        switch self {
        case .cashu: "/pay \(input)" // the core re-checks it
        case .bitcoin where !input.lowercased().hasPrefix("bitcoin:"): "bitcoin:\(input)"
        default: input
        }
    }
}

struct LinkPaymentSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    let payment: LinkPayment
    @State private var input = ""
    @State private var confirming = false
    @State private var failure: String?

    private var trimmed: String { input.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// What the core makes of it; nil until it's valid.
    private var preview: PayLink? {
        guard !trimmed.isEmpty else { return nil }
        if payment == .cashu {
            guard let token = payToken(input: trimmed) else { return nil }
            return payScan(text: token).links.first
        }
        return payScan(text: payment.body(trimmed)).links.first { $0.kind == payment.kind }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(payment.placeholder, text: $input, axis: .vertical)
                        .lineLimit(3...6)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.callout.monospaced())
                    Button("Paste") { input = UIPasteboard.general.string ?? "" }
                } footer: {
                    Text(payment.help)
                }
                if let p = preview {
                    Section("Will send") {
                        Text(p.label)
                        if let memo = p.memo { Text(memo).foregroundStyle(.secondary) }
                    }
                } else if !trimmed.isEmpty {
                    Section { Text("That doesn't check out. Paste it again from your wallet.").foregroundStyle(.red) }
                }
            }
            .navigationTitle(payment.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send") { payment == .cashu ? (confirming = true) : send() }
                        .disabled(preview == nil)
                }
            }
            .alert("Send \(preview?.label ?? "this token")?", isPresented: $confirming) {
                Button("Send", role: .destructive) { send() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(cashuBearerWarning())
            }
            .alert("Not sent", isPresented: Binding(get: { failure != nil }, set: { if !$0 { failure = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(failure ?? "")
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func send() {
        if let why = model.send(payment.body(trimmed)) {
            failure = why
        } else {
            dismiss()
        }
    }
}

/// A wallet account up close: QR of its address, copy, share.
struct AccountSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    let account: FfiTaoAccount
    let active: Bool

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                QRCode(text: account.address)
                    .frame(width: 240, height: 240)
                    .padding(16)
                    .background(RoundedRectangle(cornerRadius: 20).fill(.white))
                // Split by hand: wrapping lets iOS hyphenate the address,
                // and a stray "-" copied by hand would make it invalid.
                let mid = account.address.index(account.address.startIndex, offsetBy: account.address.count / 2)
                Text("\(account.address[..<mid])\n\(account.address[mid...])")
                    .font(.callout.monospaced())
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
                if let b = account.balance { Text(b).font(.title2.weight(.semibold)).monospacedDigit() }
                HStack(spacing: 12) {
                    Button {
                        UIPasteboard.general.string = account.address
                    } label: {
                        Label("Copy", systemImage: "doc.on.doc")
                    }
                    .buttonStyle(.bordered)
                    ShareLink(item: account.address) { Label("Share", systemImage: "square.and.arrow.up") }
                        .buttonStyle(.bordered)
                    if !active {
                        Button("Pay From This") {
                            model.taoUse(account.address)
                            dismiss()
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(account.watchOnly)
                    }
                }
                if let meta = account.meta, model.settings.privatePayments {
                    VStack(spacing: 6) {
                        Text("Private address").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        // Fixed-width lines: wrapping would hyphenate it.
                        Text(stride(from: 0, to: meta.count, by: 32).map { i in
                            String(meta.dropFirst(i).prefix(32))
                        }.joined(separator: "\n"))
                            .font(.caption.monospaced())
                            .multilineTextAlignment(.center)
                            .fixedSize()
                        HStack(spacing: 12) {
                            Button { UIPasteboard.general.string = meta } label: { Label("Copy", systemImage: "doc.on.doc") }
                            ShareLink(item: meta) { Label("Share", systemImage: "square.and.arrow.up") }
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                    }
                }
                if account.watchOnly {
                    Text("Watch-only: this shows the balance and receives TAO, but can't send.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal)
                }
                Spacer()
            }
            .padding(.top, 24)
            .navigationTitle(account.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}

/// A crisp QR code (CoreImage, scaled without smoothing).
struct QRCode: View {
    let text: String

    var body: some View {
        if let image = Self.render(text) {
            Image(uiImage: image).interpolation(.none).resizable().scaledToFit()
                .accessibilityLabel("QR code for \(text)")
        }
    }

    private static func render(_ text: String) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let out = filter.outputImage,
              let cg = CIContext().createCGImage(out, from: out.extent) else { return nil }
        return UIImage(cgImage: cg)
    }
}
