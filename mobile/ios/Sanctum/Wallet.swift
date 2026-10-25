import SwiftUI

/// Settings → TAO Wallet: accounts and balances, create / import / watch,
/// recovery phrase (behind a warning), network.
struct WalletScreen: View {
    @EnvironmentObject var model: AppModel
    @State private var showImport = false
    @State private var importText = ""
    @State private var showWatch = false
    @State private var watchText = ""
    @State private var revealFor: String?
    @State private var forgetFor: String?
    @State private var shown: FfiTaoAccount?

    var body: some View {
        Form {
            if let w = model.wallet, !w.accounts.isEmpty {
                Section {
                    ForEach(w.accounts, id: \.address) { a in
                        accountRow(a, active: w.active == a.address)
                    }
                } header: {
                    Text(w.testnet ? "Testnet" : "Mainnet")
                } footer: {
                    Text("Tap an account for its QR code. Payments in chats come from the checked one; watch-only accounts show a balance but can't send.")
                }
            } else {
                Section {
                    Text("No accounts yet").foregroundStyle(.secondary)
                } footer: {
                    Text("Create one, restore one from its recovery phrase, or watch an address whose keys live elsewhere (btcli, Talisman).")
                }
            }
            Section {
                Button("Create Account") { model.taoNew() }
                Button("Import Recovery Phrase") { showImport = true }
                Button("Watch an Address") { showWatch = true }
            }
            Section {
                Toggle("Testnet", isOn: Binding(
                    get: { model.wallet?.testnet ?? false },
                    set: { model.taoSetTestnet($0) }
                ))
            } footer: {
                Text("Mainnet moves real TAO.")
            }
        }
        .navigationTitle("TAO Wallet")
        .onAppear { model.taoRefresh() }
        .refreshable { model.taoRefresh() }
        .alert("Import account", isPresented: $showImport) {
            TextField("12 or 24 words", text: $importText)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Import") { model.taoImport(importText); importText = "" }
            Button("Cancel", role: .cancel) { importText = "" }
        } message: {
            Text("Paste the account's recovery phrase. It stays encrypted on this device.")
        }
        .alert("Watch an address", isPresented: $showWatch) {
            TextField("5F…", text: $watchText)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Add") { model.taoWatch(watchText); watchText = "" }
            Button("Cancel", role: .cancel) { watchText = "" }
        }
        .alert("Show recovery phrase?", isPresented: binding($revealFor)) {
            Button("Show", role: .destructive) { if let a = revealFor { model.taoReveal(a) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Anyone who sees these words can take this account's TAO. Make sure no one is watching your screen.")
        }
        .alert("Forget this account?", isPresented: binding($forgetFor)) {
            Button("Forget", role: .destructive) { if let a = forgetFor { model.taoForget(a) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Its keys are deleted from this device. Without its recovery phrase, its TAO can't be recovered.")
        }
        .sheet(item: $model.phrase) { p in PhraseSheet(phrase: p) }
        .sheet(item: $shown) { a in AccountSheet(account: a, active: model.wallet?.active == a.address) }
    }

    private func accountRow(_ a: FfiTaoAccount, active: Bool) -> some View {
        Button {
            shown = a
        } label: {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(a.name + (a.watchOnly ? " · watch-only" : ""))
                        .foregroundStyle(.primary)
                    Text(a.address)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
                Text(a.balance ?? "…").monospacedDigit().foregroundStyle(.primary)
                if active { Image(systemName: "checkmark").foregroundStyle(Color.accentColor) }
            }
        }
        .tint(.primary) // a row, not a link
        .contextMenu {
            Button { UIPasteboard.general.string = a.address } label: {
                Label("Copy Address", systemImage: "doc.on.doc")
            }
            if !active && !a.watchOnly {
                Button { model.taoUse(a.address) } label: {
                    Label("Pay From This Account", systemImage: "checkmark.circle")
                }
            }
            if !a.watchOnly {
                Button { revealFor = a.address } label: {
                    Label("Show Recovery Phrase", systemImage: "key")
                }
            }
            Button(role: .destructive) { forgetFor = a.address } label: {
                Label("Forget Account", systemImage: "trash")
            }
        }
    }

    private func binding(_ s: Binding<String?>) -> Binding<Bool> {
        Binding(get: { s.wrappedValue != nil }, set: { if !$0 { s.wrappedValue = nil } })
    }
}

/// TAO's τ in the circled style of SF Symbols' bitcoinsign.circle (there's
/// no SF Symbol for TAO), so ₿ and τ read as a pair.
struct TaoSymbol: View {
    @ScaledMetric(relativeTo: .body) private var size: CGFloat = 12

    var body: some View {
        Image(systemName: "circle")
            .overlay {
                Text("τ").font(.system(size: size, weight: .semibold, design: .rounded))
            }
            .accessibilityLabel("TAO")
    }
}

extension FfiTaoAccount: Identifiable {
    public var id: String { address }
}

struct PhraseVM: Identifiable {
    let address: String
    let words: [String]
    var id: String { address }
}

/// The recovery phrase, numbered. Cleared from memory when dismissed.
struct PhraseSheet: View {
    @Environment(\.dismiss) var dismiss
    let phrase: PhraseVM

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                Text("Write these down in order and keep them offline. They are the only way to restore this account.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 10) {
                    ForEach(Array(phrase.words.enumerated()), id: \.offset) { i, w in
                        HStack {
                            Text("\(i + 1).").foregroundStyle(.secondary).frame(width: 28, alignment: .trailing)
                            Text(w).font(.body.monospaced())
                            Spacer()
                        }
                    }
                }
                .privacySensitive()
                Spacer()
            }
            .padding()
            .navigationTitle("Recovery Phrase")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}

/// Send / request / cheque from a chat. Sends and cheques confirm first.
enum TaoAction: String, Identifiable {
    case send, request, cheque
    var id: String { rawValue }
    var title: String {
        switch self {
        case .send: "Send TAO"
        case .request: "Request TAO"
        case .cheque: "TAO Cheque"
        }
    }
}

struct TaoSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    let chatId: Int64
    let action: TaoAction
    @State private var amount = ""
    @State private var address = ""
    @State private var memo = ""
    @State private var confirming = false

    private var valid: Bool {
        taoParseAmount(input: amount) != nil && (address.isEmpty || taoValidAddress(input: address))
    }
    private var net: String { model.wallet?.testnet == true ? "Testnet" : "Mainnet" }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Amount", text: $amount).keyboardType(.decimalPad)
                } footer: {
                    Text("In TAO, up to 9 decimals.")
                }
                if action == .send {
                    Section {
                        TextField("Their shared address", text: $address)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.callout.monospaced())
                    } footer: {
                        Text("Leave empty to pay the address they shared in this chat.")
                    }
                }
                if action == .request {
                    Section { TextField("What for (optional)", text: $memo) }
                }
                if action == .cheque {
                    Section {} footer: {
                        Text("Signed on this phone, no network needed. Whoever gets online first cashes it. It bounces if your balance runs out first.")
                    }
                }
            }
            .navigationTitle(action.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(action == .request ? "Request" : "Review") {
                        if action == .request {
                            _ = model.taoRequest(chatId, amount: amount, memo: memo.isEmpty ? nil : memo)
                            dismiss()
                        } else {
                            confirming = true
                        }
                    }
                    .disabled(!valid)
                }
            }
            .alert(action == .send ? "Send \(amount) τ?" : "Sign a \(amount) τ cheque?", isPresented: $confirming) {
                Button(action == .send ? "Send" : "Sign", role: .destructive) {
                    if action == .send {
                        _ = model.taoPay(chatId, amount: amount, to: address.isEmpty ? nil : address, re: nil)
                    } else {
                        _ = model.taoCheque(chatId, amount: amount)
                    }
                    dismiss()
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("On \(net), to \(address.isEmpty ? "the address they shared" : address). Payments can't be undone.")
            }
        }
        .presentationDetents([.medium, .large])
    }
}

/// A payment in the chat: amount, live status, and Pay / Cash when it's theirs.
struct PayCardView: View {
    @EnvironmentObject var model: AppModel
    let chatId: Int64
    let wireId: String
    let mine: Bool
    let card: FfiPayCard
    @State private var confirmPay = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text("τ").font(.headline)
                Text(title).font(.caption.weight(.semibold)).textCase(.uppercase)
                if card.testnet { Text("testnet").font(.caption2).foregroundStyle(.secondary) }
            }
            .foregroundStyle(.secondary)
            Text(card.amount).font(.title2.weight(.semibold)).monospacedDigit()
            if let memo = card.memo { Text(memo).font(.callout) }
            Label(statusText, systemImage: statusIcon)
                .font(.caption)
                .foregroundStyle(statusColor)
            if let tx = card.tx {
                Text(tx.prefix(10) + "…" + tx.suffix(6))
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            if !mine && card.kind == "request" && card.status == "open" {
                Button("Pay \(card.amount)") { confirmPay = true }
                    .buttonStyle(.borderedProminent)
            }
            if !mine && card.kind == "cheque" && (card.status == "pending" || card.status == "bounced") {
                Button(card.status == "bounced" ? "Try Cashing Again" : "Cash Cheque") {
                    model.taoCash(chatId, wireId: wireId)
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .padding(12)
        .frame(minWidth: 180, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 18).fill(Color(.secondarySystemBackground)))
        .overlay(RoundedRectangle(cornerRadius: 18).stroke(Color.accentColor.opacity(0.35)))
        .alert("Pay \(card.amount)?", isPresented: $confirmPay) {
            Button("Pay", role: .destructive) {
                _ = model.taoPay(chatId, amount: card.amount, to: card.to, re: wireId)
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("On \(card.testnet ? "Testnet" : "Mainnet") to \(card.to). Payments can't be undone.")
        }
    }

    private var title: String {
        switch card.kind {
        case "request": mine ? "You requested" : "Request"
        case "cheque": "Cheque"
        default: mine ? "You paid" : "Payment"
        }
    }

    private var statusText: String {
        switch card.status {
        case "sending": "Sending to the chain…"
        case "sent": "Sent · finalized on chain"
        case "verifying": "Checking on chain…"
        case "verified": "Verified on chain"
        case "unverified": "Not found on chain"
        case "failed": "Failed" + (card.note.map { ": \($0)" } ?? "")
        case "open": mine ? "Waiting for payment" : "Unpaid"
        case "settled": "Paid"
        case "pending": mine ? "Not cashed yet" : "Cash it when you're online"
        case "cashing": "Cashing…"
        case "cashed": "Cashed"
        case "bounced": "Bounced" + (card.note.map { ": \($0)" } ?? "")
        default: card.status
        }
    }

    private var statusIcon: String {
        switch card.status {
        case "sent", "verified", "settled", "cashed": "checkmark.seal.fill"
        case "failed", "unverified", "bounced": "exclamationmark.triangle.fill"
        default: "clock"
        }
    }

    private var statusColor: Color {
        switch card.status {
        case "sent", "verified", "settled", "cashed": .green
        case "failed", "unverified", "bounced": .red
        default: .secondary
        }
    }
}
