import SwiftUI

/// Settings → Mailbox Account: the Hippius account that pays for the chats
/// you start. Set up from nothing but a recovery phrase made on this device,
/// funded with TAO; joining someone's invite never needs one.
struct MailboxScreen: View {
    @EnvironmentObject var model: AppModel
    @State private var restoring = false
    @State private var restoreText = ""
    @State private var token = ""
    @State private var confirmFund = false
    @State private var revealing = false

    private var mailbox: FfiMailbox? { model.wallet?.mailbox }

    /// The wallet account a top-up comes from.
    private var payer: FfiTaoAccount? {
        model.wallet.flatMap { w in w.accounts.first { $0.address == w.active && !$0.watchOnly } }
    }

    var body: some View {
        Form {
            if let m = mailbox {
                if m.busy && m.credits == nil { settingUp(m) } else { account(m) }
            } else {
                intro
            }
        }
        .navigationTitle("Mailbox Account")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { model.taoRefresh() }
        .refreshable { model.taoRefresh() }
        // Credits arrive a few minutes after a deposit: keep checking while
        // this screen is open and the account isn't funded yet.
        .task(id: mailbox?.funded) {
            while mailbox != nil, mailbox?.funded == false, !Task.isCancelled {
                try? await Task.sleep(for: .seconds(30))
                model.taoRefresh()
            }
        }
        .alert("Restore account", isPresented: $restoring) {
            TextField("12 words", text: $restoreText)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Restore") { model.mailboxOpen(restoreText); restoreText = "" }
            Button("Cancel", role: .cancel) { restoreText = "" }
        } message: {
            Text("Paste your mailbox account's recovery phrase. It stays encrypted on this device.")
        }
        .alert("Add $20 of credits?", isPresented: $confirmFund) {
            Button("Send") { model.mailboxFund() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Sends \(mailbox?.fundTao ?? "TAO") from \(payer?.name ?? "your wallet") to your Hippius deposit address on mainnet. TAO transfers are final.")
        }
        .alert("Show recovery phrase?", isPresented: $revealing) {
            Button("Show", role: .destructive) { model.mailboxReveal() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Anyone who sees these words controls your mailbox account and its credits. Make sure no one is watching your screen.")
        }
        .sheet(item: $model.phrase) { p in PhraseSheet(phrase: p) }
    }

    private var intro: some View {
        Group {
            Section {
                VStack(spacing: 10) {
                    Image(systemName: "tray.full")
                        .font(.system(size: 40))
                        .foregroundStyle(.tint)
                    Text("Your Mailbox Account").font(.title3.bold())
                    Text("Chats you start get private mailboxes on Hippius, paid from this account. Joining someone's invite never needs one.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity)
                .padding(.top, 8)
                .listRowSeparator(.hidden)
                Button { model.mailboxOpen() } label: {
                    Text("Set Up Account").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .padding(.bottom, 8)
            } footer: {
                Text("No email or phone number: the account is a recovery phrase, kept encrypted on this device. Then add $20 of TAO and you're set.")
            }
            Section {
                Button("Restore from Recovery Phrase") { restoring = true }
                SecureField("Hippius API token", text: $token)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Use Token") {
                    model.saveToken(token)
                    token = ""
                    model.taoRefresh()
                }
                .disabled(token.trimmingCharacters(in: .whitespaces).isEmpty)
            } header: {
                Text("Already have one?")
            } footer: {
                Text("A token from console.hippius.com → Settings works too. It stays on this device.")
            }
        }
    }

    private func settingUp(_ m: FfiMailbox) -> some View {
        Section {
            HStack(spacing: 12) {
                ProgressView()
                Text(m.note ?? "Setting up your account…")
            }
        } footer: {
            Text("This takes a few seconds.")
        }
    }

    @ViewBuilder
    private func account(_ m: FfiMailbox) -> some View {
        Section {
            VStack(spacing: 6) {
                Text(m.credits ?? "…")
                    .font(.system(size: 44, weight: .semibold, design: .rounded))
                    .monospacedDigit()
                    .contentTransition(.numericText())
                Label(
                    m.funded ? "Ready to start chats" : "Add $20 of TAO to start chats",
                    systemImage: m.funded ? "checkmark.circle.fill" : "exclamationmark.circle"
                )
                .font(.subheadline)
                .foregroundStyle(m.funded ? .green : .orange)
                if let note = m.note {
                    HStack(spacing: 6) {
                        if m.busy { ProgressView().controlSize(.small) }
                        Text(note)
                    }
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.top, 4)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            .animation(.default, value: m.credits)
        } header: {
            Text("Credits")
        }
        if let payer, let tao = m.fundTao, m.deposit != nil {
            Section {
                Button { confirmFund = true } label: {
                    Label { Text("Add $20 from TAO Wallet") } icon: { TaoSymbol() }
                }
                .disabled(m.busy)
            } footer: {
                Text("\(tao) from \(payer.name), on mainnet.")
            }
        }
        if let d = m.deposit {
            Section {
                VStack(spacing: 14) {
                    QRCode(text: d)
                        .frame(width: 170, height: 170)
                        .padding(12)
                        .background(RoundedRectangle(cornerRadius: 16).fill(.white))
                    // Split by hand: wrapping lets iOS hyphenate the address.
                    let mid = d.index(d.startIndex, offsetBy: d.count / 2)
                    Text("\(d[..<mid])\n\(d[mid...])")
                        .font(.callout.monospaced())
                        .multilineTextAlignment(.center)
                        .textSelection(.enabled)
                    HStack(spacing: 12) {
                        Button { UIPasteboard.general.string = d } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
                        ShareLink(item: d) { Label("Share", systemImage: "square.and.arrow.up") }
                    }
                    .buttonStyle(.bordered)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
            } header: {
                Text("Deposit address")
            } footer: {
                Text("Send at least $20 of TAO here from any wallet; credits show up within a few minutes. Never send to the account address below: it isn't credited.")
            }
        }
        Section {
            if m.address != nil {
                Button("Show Recovery Phrase") { revealing = true }
            }
        } footer: {
            if let a = m.address {
                Text("Account \(a). Its recovery phrase restores it on another device; back it up, since an emergency wipe erases it from this one.")
            } else {
                Text("Connected with an API token.")
            }
        }
    }
}
