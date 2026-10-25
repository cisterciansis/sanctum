import SwiftUI
import PhotosUI
import UniformTypeIdentifiers
import CoreImage.CIFilterBuiltins
import VisionKit

let tapbacks = ["👍", "👎", "❤️", "‼️", "❓", "😂"]

/// An emoji drawn into an image, for menus that only take images.
func emojiImage(_ emoji: String) -> UIImage {
    let text = emoji as NSString
    let attrs: [NSAttributedString.Key: Any] = [.font: UIFont.systemFont(ofSize: 26)]
    let size = text.size(withAttributes: attrs)
    return UIGraphicsImageRenderer(size: size).image { _ in text.draw(at: .zero, withAttributes: attrs) }
}

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    @State private var showJoin = false
    @State private var showSettings = false
    @State private var showMailbox = false
    @State private var showNewGroup = false
    @State private var renaming: ChatVM?
    @State private var renameText = ""
    @State private var wipePulse = 0

    var body: some View {
        NavigationSplitView {
            List(selection: $model.selected) {
                ForEach(model.listed) { chat in
                    ChatRow(chat: chat)
                        .tag(chat.id)
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button(role: .destructive) {
                                model.deleteChat(chat.id)
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                            Button {
                                renameText = chat.name
                                renaming = chat
                            } label: {
                                Label("Rename", systemImage: "pencil")
                            }
                            .tint(.indigo)
                        }
                        .contextMenu {
                            Button {
                                renameText = chat.name
                                renaming = chat
                            } label: {
                                Label("Rename", systemImage: "pencil")
                            }
                            if !chat.openInvites.isEmpty {
                                if !chat.isGroupInvite, let only = chat.openInvites.first {
                                    Button {
                                        UIPasteboard.general.string = only.link
                                    } label: {
                                        Label("Copy Invite Link", systemImage: "link")
                                    }
                                } else {
                                    Menu {
                                        ForEach(chat.openInvites, id: \.n) { seat in
                                            Button("Member \(seat.n + 2)") {
                                                UIPasteboard.general.string = seat.link
                                            }
                                        }
                                    } label: {
                                        Label("Copy Invite…", systemImage: "link")
                                    }
                                }
                            }
                            Button {
                                model.clearHistory(chat.id, olderThanSecs: nil)
                            } label: {
                                Label("Clear History", systemImage: "eraser")
                            }
                            Divider()
                            Button(role: .destructive) {
                                model.deleteChat(chat.id)
                            } label: {
                                Label("Delete Chat", systemImage: "trash")
                            }
                        }
                }
                if model.inviting {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text("Inviting…").foregroundStyle(.secondary)
                    }
                    .font(.subheadline)
                }
            }
            .navigationTitle("Chats")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                // Emergency wipe, as in bitchat: triple-tap the title. No
                // confirmation by design; it's for when there's no time.
                ToolbarItem(placement: .principal) {
                    Text("Chats")
                        .font(.headline)
                        .onTapGesture(count: 3) {
                            model.wipe()
                            wipePulse += 1
                        }
                        .sensoryFeedback(.warning, trigger: wipePulse)
                        .accessibilityHint("Triple-tap to erase everything on this device")
                }
                ToolbarItem(placement: .topBarLeading) {
                    Button { showSettings = true } label: {
                        Image(systemName: "gearshape")
                    }
                }
                ToolbarItem(placement: .primaryAction) {
                    Menu {
                        Button {
                            if model.needsMailbox() {
                                showMailbox = true
                            } else {
                                model.newInvite()
                            }
                        } label: {
                            Label("New Invite", systemImage: "person.badge.plus")
                        }
                        Button {
                            if model.needsMailbox() {
                                showMailbox = true
                            } else {
                                showNewGroup = true
                            }
                        } label: {
                            Label("New Group", systemImage: "person.3")
                        }
                        Button { showJoin = true } label: {
                            Label("Join with Link", systemImage: "link.badge.plus")
                        }
                    } label: {
                        Image(systemName: "plus")
                    }
                }
            }
            .overlay {
                if model.chats.isEmpty && !model.inviting {
                    ContentUnavailableView(
                        "No chats yet",
                        systemImage: "lock.shield",
                        description: Text("Create an invite, or join one with a link.")
                    )
                }
            }
            .safeAreaInset(edge: .bottom) {
                if model.status.hasPrefix("error") {
                    Text(model.status)
                        .font(.caption2)
                        .foregroundStyle(.red)
                        .frame(maxWidth: .infinity)
                        .padding(6)
                        .background(.bar)
                } else if let tor = model.tor, !tor.ready {
                    Label(tor.message, systemImage: "network.badge.shield.half.filled")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                        .padding(6)
                        .background(.bar)
                }
            }
        } detail: {
            if let sel = model.selected,
               let chat = model.chats.first(where: { $0.id == sel }) {
                ChatView(chat: chat)
            } else {
                Text("Select a chat").foregroundStyle(.secondary)
            }
        }
        .sheet(isPresented: $showJoin) { JoinSheet() }
        .sheet(isPresented: $showSettings) { SettingsSheet() }
        .sheet(isPresented: $showMailbox) {
            NavigationStack {
                MailboxScreen()
                    .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { showMailbox = false } } }
            }
        }
        .sheet(isPresented: $showNewGroup) { NewGroupSheet() }
        .preferredColorScheme(model.colorScheme)
        .alert("Rename chat", isPresented: Binding(
            get: { renaming != nil },
            set: { if !$0 { renaming = nil } }
        )) {
            TextField("Name", text: $renameText)
            Button("Save") {
                if let chat = renaming {
                    model.rename(chat.id, to: renameText)
                }
                renaming = nil
            }
            Button("Cancel", role: .cancel) { renaming = nil }
        }
    }
}

struct ChatRow: View {
    @EnvironmentObject var model: AppModel
    let chat: ChatVM

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    if chat.favorite {
                        Image(systemName: "star.fill").font(.caption2).foregroundStyle(.yellow)
                    }
                    Text(chat.name)
                    if chat.verified {
                        Image(systemName: "checkmark.seal.fill").font(.caption).foregroundStyle(.green)
                    }
                }
                if let last = chat.msgs.last {
                    Text(last.display ?? last.text)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                } else if chat.waiting {
                    Text("Invitation pending")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            if chat.keyChanged {
                Image(systemName: "exclamationmark.shield.fill").foregroundStyle(.red)
            } else if chat.blocked {
                Image(systemName: "hand.raised.fill").foregroundStyle(.secondary)
            } else if chat.waiting {
                Circle().fill(.quaternary).frame(width: 8, height: 8)
            } else if chat.unread > 0 {
                Text("\(chat.unread)")
                    .font(.caption2).bold()
                    .padding(.horizontal, 6).padding(.vertical, 2)
                    .background(Capsule().fill(Color.accentColor))
                    .foregroundStyle(.white)
            }
        }
    }
}

struct JoinSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    @State private var link = ""
    @State private var waiting = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("Invite link") {
                    TextField("sanctum://…", text: $link, axis: .vertical)
                        .lineLimit(3...6)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
                Section {
                    if waiting {
                        HStack(spacing: 10) {
                            ProgressView()
                            Text("Joining…").foregroundStyle(.secondary)
                        }
                    } else {
                        Button("Join") {
                            error = nil
                            waiting = true
                            model.join(link)
                        }
                        .disabled({
                            let t = link.trimmingCharacters(in: .whitespacesAndNewlines)
                            return !(t.hasPrefix("sanctum://v2/") || t.hasPrefix("sanctum://g1/"))
                        }())
                    }
                } footer: {
                    if let error {
                        Text(error).foregroundStyle(.red)
                    } else {
                        Text("An invite carries everything you need — no account, no signup.")
                    }
                }
            }
            .navigationTitle("Join a chat")
            .preferredColorScheme(model.colorScheme)
            .onChange(of: model.chats.count) {
                if waiting { dismiss() }
            }
            .onChange(of: model.status) {
                if waiting && model.status.hasPrefix("error") {
                    waiting = false
                    error = model.status
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

struct NewGroupSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    @State private var name = ""
    @State private var size = 3

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Group name", text: $name)
                    Stepper("Members: \(size)", value: $size, in: 2...6)
                } footer: {
                    Text("You get one invite link per member. Each is single-use; nobody needs an account.")
                }
                Section {
                    Button("Create Group") {
                        model.newGroup(name: name, size: Int64(size))
                        dismiss()
                    }
                }
            }
            .navigationTitle("New Group")
            .preferredColorScheme(model.colorScheme)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

enum Erase: Identifiable {
    case keepWallets, everything
    var id: Self { self }
}

struct SettingsSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    @State private var erase: Erase?

    var body: some View {
        NavigationStack {
            Form {
                Section("Appearance") {
                    Picker("Theme", selection: Binding(
                        get: { model.settings.theme },
                        set: { model.settings.theme = $0; model.saveSettings() }
                    )) {
                        Text("System").tag("system")
                        Text("Light").tag("light")
                        Text("Dark").tag("dark")
                    }
                    .pickerStyle(.segmented)
                }
                Section {
                    Toggle("Timestamps", isOn: Binding(
                        get: { model.settings.timestamps },
                        set: { model.settings.timestamps = $0; model.saveSettings() }
                    ))
                    // "tor" = Tor built into the app; there is no local
                    // proxy to point at on iOS.
                    Toggle("Route via Tor", isOn: Binding(
                        get: { model.settings.proxyEnabled },
                        set: {
                            model.settings.proxyEnabled = $0
                            model.settings.proxyUrl = "tor"
                            model.saveSettings()
                        }
                    ))
                } header: {
                    Text("Messages & network")
                } footer: {
                    switch (model.settings.proxyEnabled, model.tor) {
                    case (true, let tor?): Text(tor.message)
                    case (true, nil): Text("Tor starts on next launch.")
                    case (false, _?): Text("Direct connection on next launch.")
                    case (false, nil): Text("Hides your IP address from the storage network. Slower, and the first connect can take up to a minute.")
                    }
                }
                Section {
                    NavigationLink {
                        MailboxScreen()
                    } label: {
                        LabeledContent {
                            Text(model.wallet?.mailbox.map { $0.credits ?? "…" } ?? "Set Up")
                        } label: {
                            Label("Mailbox Account", systemImage: "tray.full")
                        }
                    }
                    NavigationLink {
                        WalletScreen()
                    } label: {
                        Label { Text("TAO Wallet") } icon: { TaoSymbol() }
                    }
                } footer: {
                    Text("Your mailbox account pays for the chats you start. Joining someone's invite never needs one.")
                }
                Section {
                    Toggle("Nearby (Bluetooth)", isOn: Binding(
                        get: { model.meshOn },
                        set: { model.setMesh($0) }
                    ))
                } footer: {
                    if model.meshOn {
                        Text("\(model.meshLinks) nearby link\(model.meshLinks == 1 ? "" : "s"). Chats try Bluetooth first, relayed by up to 7 Sanctum devices, still end-to-end encrypted.")
                    } else {
                        Text("Message nearby contacts without internet. Others around can tell a Sanctum user is near, never who or what.")
                    }
                }
                Section {
                    Toggle("Read Receipts", isOn: Binding(
                        get: { model.settings.readReceipts },
                        set: { model.settings.readReceipts = $0; model.saveSettings() }
                    ))
                } footer: {
                    Text("Let people know when you've read their messages. When off, you won't see theirs either.")
                }
                Section {
                    Button("Erase Chats, Keep Wallets", role: .destructive) { erase = .keepWallets }
                    Button("Erase Everything", role: .destructive) { erase = .everything }
                } header: {
                    Text("Emergency wipe")
                } footer: {
                    Text("Instant and offline. Triple-tapping the Chats title erases everything too.")
                }
                .confirmationDialog(
                    erase == .everything ? "Erase everything?" : "Erase chats, keep wallets?",
                    isPresented: Binding(get: { erase != nil }, set: { if !$0 { erase = nil } }),
                    titleVisibility: .visible,
                    presenting: erase
                ) { e in
                    Button(e == .everything ? "Erase Everything" : "Erase Chats", role: .destructive) {
                        model.wipe(keepWallets: e == .keepWallets)
                        dismiss()
                    }
                } message: { e in
                    switch e {
                    case .keepWallets:
                        Text("Every chat, contact, key, photo and setting is deleted from this device. Your TAO wallet and mailbox account stay, so no money is lost.")
                    case .everything:
                        Text("Every trace of Sanctum on this device is deleted, wallets included. Their funds are gone unless you saved the recovery phrases.")
                    }
                }
                Section {
                } footer: {
                    Text("End-to-end encrypted · mailboxes on Hippius (Bittensor SN75)")
                }
            }
            .navigationTitle("Settings")
            .preferredColorScheme(model.colorScheme)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

struct ChatView: View {
    @EnvironmentObject var model: AppModel
    let chat: ChatVM
    @State private var draft = ""
    @State private var copied = false
    @State private var sendPulse = 0
    @State private var photoItem: PhotosPickerItem?
    @State private var showPhotoPicker = false
    @State private var showFileImporter = false
    @State private var taoAction: TaoAction?
    @State private var linkPayment: LinkPayment?
    @State private var showRename = false
    @State private var renameText = ""
    @State private var showCustomTimer = false
    @State private var customTimer = ""
    @State private var timerInvalid = false
    @State private var showSafety = false
    @State private var sendError: String?

    @State private var reveal: CGFloat = 0
    /// The end-of-list marker is on screen, so new messages may scroll in.
    @State private var atBottom = true

    /// Sent → Delivered → Read <time> (Read only while receipts are on:
    /// they're reciprocal).
    private func receiptLabel(_ msg: MsgVM) -> String {
        if let at = msg.readAt, model.settings.readReceipts {
            return "Read " + at.formatted(date: .omitted, time: .shortened)
        }
        return msg.delivered ? "Delivered" : "Sent"
    }

    /// Presets and labels come from the core, so every client matches.
    private var timerMenu: some View {
        Menu {
            let current = model.settings.disappearSecs
            let presets = disappearPresets()
            ForEach(presets, id: \.self) { secs in
                Button {
                    model.setDisappear(secs)
                } label: {
                    if current == secs {
                        Label(disappearLabel(secs: secs), systemImage: "checkmark")
                    } else {
                        Text(disappearLabel(secs: secs))
                    }
                }
            }
            Button {
                customTimer = ""
                showCustomTimer = true
            } label: {
                if presets.contains(current) {
                    Text("Custom…")
                } else {
                    Label("Custom: \(disappearLabel(secs: current))", systemImage: "checkmark")
                }
            }
        } label: {
            Label("Disappearing Messages", systemImage: "timer")
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            if chat.keyChanged {
                KeyChangedBanner { showSafety = true }
            }
            if !chat.openInvites.isEmpty {
                InviteBanner(seats: chat.openInvites, isGroup: chat.isGroupInvite)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 4) {
                        // Only the newest finished send carries Sent / Delivered.
                        let lastSent = chat.msgs.last { $0.mine && $0.sent && !$0.failed }
                        ForEach(Array(chat.msgs.enumerated()), id: \.element.id) { i, msg in
                            let prev = i > 0 ? chat.msgs[i - 1] : nil
                            let newDay = prev.map {
                                !Calendar.current.isDate($0.ts, inSameDayAs: msg.ts)
                            } ?? true
                            let gap = prev.map { msg.ts.timeIntervalSince($0.ts) > 300 } ?? true
                            if newDay || (model.settings.timestamps && gap) {
                                Text(timeHeader(msg.ts))
                                    .font(.caption2.weight(.medium))
                                    .foregroundStyle(.secondary)
                                    .frame(maxWidth: .infinity)
                                    .padding(.top, 10)
                                    .padding(.bottom, 2)
                            }
                            let sameSender = prev.map {
                                !$0.mine && !msg.mine && $0.senderSeat == msg.senderSeat
                                    && !gap && !newDay
                            } ?? false
                            // Swipe left anywhere to slide every bubble over and
                            // reveal exact send times (iMessage gesture).
                            ZStack(alignment: .trailing) {
                                Bubble(
                                    chatId: chat.id, msg: msg, showSender: !sameSender,
                                    receipt: msg.id == lastSent?.id ? receiptLabel(msg) : nil
                                )
                                    .offset(x: reveal)
                                Text(msg.ts.formatted(date: .omitted, time: .shortened))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                                    .fixedSize()
                                    .offset(x: 72 + reveal)
                                    .opacity(Double(min(1, -reveal / 50)))
                            }
                            .transition(.scale(scale: 0.96).combined(with: .opacity))
                        }
                        Color.clear.frame(height: 1)
                            .onAppear { atBottom = true }
                            .onDisappear { atBottom = false }
                    }
                    .padding(12)
                }
                .scrollDismissesKeyboard(.interactively)
                .simultaneousGesture(
                    DragGesture(minimumDistance: 15)
                        .onChanged { v in
                            guard abs(v.translation.width) > abs(v.translation.height) else { return }
                            reveal = max(-72, min(0, v.translation.width))
                        }
                        .onEnded { _ in
                            withAnimation(.spring(duration: 0.3, bounce: 0.1)) { reveal = 0 }
                        }
                )
                .animation(.spring(duration: 0.25, bounce: 0.15), value: chat.msgs.count)
                .onChange(of: chat.msgs.count) {
                    // Follow new messages only if the reader is already at the
                    // bottom (or just sent one); never yank them mid-scroll.
                    if let last = chat.msgs.last, atBottom || last.mine {
                        withAnimation(.spring(duration: 0.25)) {
                            proxy.scrollTo(last.id, anchor: .bottom)
                        }
                    }
                }
            }
            HStack(spacing: 8) {
                Menu {
                    Button {
                        showPhotoPicker = true
                    } label: {
                        Label("Photo Library", systemImage: "photo.on.rectangle")
                    }
                    Button {
                        showFileImporter = true
                    } label: {
                        Label("Choose File", systemImage: "folder")
                    }
                    if chat.id >= 0 {
                        Divider()
                        // Money, grouped so the attach menu stays short.
                        Menu {
                            Button { taoAction = .send } label: { Label("TAO", systemImage: "arrow.up.circle") }
                            Button { linkPayment = .cashu } label: { Label("Cashu Token", systemImage: "banknote") }
                        } label: {
                            Label("Pay", systemImage: "arrow.up.circle")
                        }
                        Menu {
                            Button { taoAction = .request } label: { Label("TAO", systemImage: "arrow.down.circle") }
                            Button { linkPayment = .lightning } label: { Label("Lightning Invoice", systemImage: "bolt.fill") }
                            Button { linkPayment = .bitcoin } label: { Label("Bitcoin Address", systemImage: "bitcoinsign.circle") }
                            Button { linkPayment = .zcash } label: { Label("Zcash Address", systemImage: "z.circle") }
                            Divider()
                            Button { model.taoShare(chat.id) } label: { Label("Share My TAO Address", systemImage: "person.crop.circle") }
                        } label: {
                            Label("Request", systemImage: "arrow.down.circle")
                        }
                        Menu {
                            Button { taoAction = .cheque } label: { Label("TAO Cheque", systemImage: "signature") }
                        } label: {
                            Label("Offline Cheque", systemImage: "signature")
                        }
                    }
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 30, height: 30)
                        .background(Circle().fill(Color(.systemGray5)))
                }
                .disabled(chat.waiting)
                .sheet(item: $taoAction) { a in TaoSheet(chatId: chat.id, action: a) }
                .sheet(item: $linkPayment) { p in LinkPaymentSheet(payment: p) }
                .photosPicker(isPresented: $showPhotoPicker, selection: $photoItem, matching: .images)
                .onChange(of: photoItem) {
                    guard let item = photoItem else { return }
                    photoItem = nil
                    Task {
                        if let data = try? await item.loadTransferable(type: Data.self) {
                            await MainActor.run {
                                model.sendFile(data, suggestedName: "photo.jpg")
                            }
                        }
                    }
                }
                .fileImporter(
                    isPresented: $showFileImporter,
                    allowedContentTypes: [.item],
                    allowsMultipleSelection: false
                ) { result in
                    guard case .success(let urls) = result, let url = urls.first else { return }
                    let scoped = url.startAccessingSecurityScopedResource()
                    defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                    if let data = try? Data(contentsOf: url) {
                        model.sendFile(data, suggestedName: url.lastPathComponent)
                    }
                }
                TextField("Sanctum", text: $draft, axis: .vertical)
                    .lineLimit(1...4)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 7)
                    .background(
                        RoundedRectangle(cornerRadius: 18)
                            .strokeBorder(Color(.systemGray4))
                    )
                Button {
                    if let error = model.send(draft) {
                        sendError = error
                    } else {
                        draft = ""
                        sendPulse += 1
                    }
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 15, weight: .bold))
                        .foregroundStyle(.white)
                        .frame(width: 32, height: 32)
                        .background(Circle().fill(Color.accentColor))
                }
                .buttonStyle(PressableStyle())
                .sensoryFeedback(.impact(weight: .light), trigger: sendPulse)
                .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty || chat.waiting)
            }
            .padding(10)
        }
        .navigationTitle(chat.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button {
                        renameText = chat.name
                        showRename = true
                    } label: {
                        Label("Rename", systemImage: "pencil")
                    }
                    if !chat.openInvites.isEmpty {
                        if !chat.isGroupInvite, let only = chat.openInvites.first {
                            Button {
                                UIPasteboard.general.string = only.link
                            } label: {
                                Label("Copy Invite Link", systemImage: "link")
                            }
                            ShareLink(item: only.link) {
                                Label("Share Invite", systemImage: "square.and.arrow.up")
                            }
                        } else {
                            Menu {
                                ForEach(chat.openInvites, id: \.n) { seat in
                                    Button("Member \(seat.n + 2)") {
                                        UIPasteboard.general.string = seat.link
                                    }
                                }
                            } label: {
                                Label("Copy Invite…", systemImage: "link")
                            }
                        }
                    }
                    if chat.id >= 0 {
                        timerMenu
                        Button { showSafety = true } label: {
                            Label(
                                chat.verified ? "Safety Number (Verified)" : "Verify Safety Number",
                                systemImage: chat.verified ? "checkmark.shield.fill" : "checkmark.shield"
                            )
                        }
                        Button { model.setFavorite(chat.id, !chat.favorite) } label: {
                            Label(chat.favorite ? "Unfavorite" : "Favorite",
                                  systemImage: chat.favorite ? "star.slash" : "star")
                        }
                        Button { model.setBlocked(chat.id, !chat.blocked) } label: {
                            Label(chat.blocked ? "Unblock" : "Block", systemImage: "hand.raised")
                        }
                    }
                    Button {
                        model.clearHistory(chat.id, olderThanSecs: nil)
                    } label: {
                        Label("Clear History", systemImage: "eraser")
                    }
                    Divider()
                    Button(role: .destructive) {
                        model.deleteChat(chat.id)
                    } label: {
                        Label(chat.id < 0 ? "Leave Group" : "Delete Chat", systemImage: "trash")
                    }
                } label: {
                    Image(systemName: "ellipsis")
                }
            }
        }
        .alert("Rename chat", isPresented: $showRename) {
            TextField("Name", text: $renameText)
            Button("Save") { model.rename(chat.id, to: renameText) }
            Button("Cancel", role: .cancel) {}
        }
        .modifier(CustomTimerAlerts(
            show: $showCustomTimer, text: $customTimer, invalid: $timerInvalid
        ) { model.setDisappear($0) })
        .modifier(ChatSheets(chatId: chat.id, showSafety: $showSafety, sendError: $sendError))
        .onAppear { model.markRead(chat.id) }
    }
}

/// Custom disappearing timer entry; the core parses and bounds-checks it.
/// (Its own modifier keeps ChatView's body within the type checker's budget.)
struct CustomTimerAlerts: ViewModifier {
    @Binding var show: Bool
    @Binding var text: String
    @Binding var invalid: Bool
    let apply: (UInt64) -> Void

    func body(content: Content) -> some View {
        content
            .alert("Custom timer", isPresented: $show) {
                TextField("90m, 2w, 6mo", text: $text)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Set") {
                    if let secs = parseDisappear(input: text) {
                        apply(secs)
                    } else {
                        invalid = true
                    }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Anything from 1 minute to 1 year.")
            }
            .alert("Invalid timer", isPresented: $invalid) {
                Button("OK", role: .cancel) {}
            } message: {
                Text("Use off, or 1 minute to 1 year, like 90m, 36h, 2w, 6mo or 1y.")
            }
    }
}

/// Safety-number sheet and the `/pay` error, kept out of ChatView's body for
/// the type checker's sake.
struct ChatSheets: ViewModifier {
    let chatId: Int64
    @Binding var showSafety: Bool
    @Binding var sendError: String?

    func body(content: Content) -> some View {
        content
            .sheet(isPresented: $showSafety) { SafetySheet(chatId: chatId) }
            .alert("Not sent", isPresented: Binding(
                get: { sendError != nil },
                set: { if !$0 { sendError = nil } }
            )) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(sendError ?? "")
            }
    }
}

struct KeyChangedBanner: View {
    let verify: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label("Identity key changed", systemImage: "exclamationmark.shield.fill")
                .font(.subheadline.bold())
                .foregroundStyle(.red)
            Text("Someone wrote to this chat under a different identity key. It was refused. Compare safety numbers in person.")
                .font(.caption)
            Button("Verify Safety Number", action: verify).font(.caption.bold())
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.red.opacity(0.1))
    }
}

/// QR payload: both phones compute the same digits, so either can scan the other.
private func safetyQR(_ safety: String) -> String { "sanctum-safety:" + safety.filter(\.isNumber) }

struct SafetySheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    let chatId: Int64
    @State private var scanning = false
    @State private var scanResult: Bool?

    var body: some View {
        let chat = model.chats.first { $0.id == chatId }
        let safety = chat?.safety ?? ""
        NavigationStack {
            Form {
                if safety.isEmpty {
                    Text("The safety number appears once they've joined.")
                        .foregroundStyle(.secondary)
                } else {
                    Section {
                        if let qr = qrImage(safetyQR(safety)) {
                            Image(uiImage: qr)
                                .interpolation(.none)
                                .resizable()
                                .scaledToFit()
                                .frame(maxWidth: 220)
                                .frame(maxWidth: .infinity)
                        }
                        Text(safety)
                            .font(.system(.title3, design: .monospaced))
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity)
                            .textSelection(.enabled)
                    } footer: {
                        Text("Together in person, scan their code or compare these digits with their screen. If they match, nobody is in the middle.")
                    }
                    Section {
                        if DataScannerViewController.isSupported {
                            Button {
                                scanResult = nil
                                scanning = true
                            } label: {
                                Label("Scan Their Code", systemImage: "qrcode.viewfinder")
                            }
                        }
                        if let ok = scanResult {
                            Label(ok ? "Numbers match. Marked verified." : "Numbers don't match. This may not be your contact.",
                                  systemImage: ok ? "checkmark.seal.fill" : "xmark.octagon.fill")
                                .foregroundStyle(ok ? .green : .red)
                        }
                        Toggle("Verified", isOn: Binding(
                            get: { chat?.verified ?? false },
                            set: { model.setVerified(chatId, $0) }
                        ))
                    } footer: {
                        Text("Kept on this device only.")
                    }
                }
            }
            .navigationTitle("Safety Number")
            .navigationBarTitleDisplayMode(.inline)
            .preferredColorScheme(model.colorScheme)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .sheet(isPresented: $scanning) {
                QRScanner { code in
                    scanning = false
                    let ok = code == safetyQR(safety)
                    scanResult = ok
                    if ok { model.setVerified(chatId, true) }
                }
                .ignoresSafeArea()
            }
            .sensoryFeedback(trigger: scanResult) { _, new in
                new.map { $0 ? .success : .error }
            }
        }
    }
}

func qrImage(_ text: String) -> UIImage? {
    let filter = CIFilter.qrCodeGenerator()
    filter.message = Data(text.utf8)
    filter.correctionLevel = "M"
    guard let out = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)),
          let cg = CIContext().createCGImage(out, from: out.extent) else { return nil }
    return UIImage(cgImage: cg)
}

/// Live camera QR reader (VisionKit); reports the first code it sees.
struct QRScanner: UIViewControllerRepresentable {
    let onScan: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let vc = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            isHighlightingEnabled: true
        )
        vc.delegate = context.coordinator
        try? vc.startScanning()
        return vc
    }

    func updateUIViewController(_ vc: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(onScan: onScan) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onScan: (String) -> Void
        private var done = false

        init(onScan: @escaping (String) -> Void) { self.onScan = onScan }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem],
                         allItems: [RecognizedItem]) {
            for case .barcode(let code) in items {
                if let value = code.payloadStringValue, !done {
                    done = true
                    onScan(value)
                }
            }
        }
    }
}

/// A Cashu token or a lightning:/bitcoin: link, handed to an external
/// wallet on tap; long-press copies it. Sanctum holds no Bitcoin keys.
struct PayChip: View {
    let link: PayLink
    let mine: Bool
    @Environment(\.openURL) private var openURL
    @State private var noWallet = false

    private var icon: String {
        switch link.kind {
        case .cashu: "banknote"
        case .lightning: "bolt.fill"
        case .bitcoin: "bitcoinsign.circle"
        case .zcash: "z.circle"
        }
    }

    var body: some View {
        VStack(alignment: mine ? .trailing : .leading, spacing: 3) {
            Button {
                if let url = URL(string: link.uri) {
                    openURL(url) { accepted in noWallet = !accepted }
                }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: icon).foregroundStyle(.orange)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(link.label).font(.footnote.weight(.semibold))
                        if let memo = link.memo {
                            Text(memo).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                        }
                    }
                }
                .padding(.vertical, 6)
                .padding(.horizontal, 12)
                .background(RoundedRectangle(cornerRadius: 12).fill(Color(.secondarySystemBackground)))
                .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color(.separator)))
            }
            .buttonStyle(.plain)
            .contextMenu {
                Button {
                    UIPasteboard.general.string = link.raw
                } label: {
                    Label("Copy", systemImage: "doc.on.doc")
                }
            }
            if link.kind == .cashu {
                Text(bearerWarning).font(.caption2).foregroundStyle(.secondary)
            }
        }
        .alert("No wallet app", isPresented: $noWallet) {
            Button("Copy") { UIPasteboard.general.string = link.raw }
            Button("OK", role: .cancel) {}
        } message: {
            Text("Nothing on this phone opens this link. Copy it into your wallet instead.")
        }
    }
}

/// Upload indicator under a sending bubble: a thin bar easing toward 90%;
/// it gives way to "Sent" once the upload finishes.
struct SendingBar: View {
    let active: Bool
    @State private var progress: CGFloat = 0
    @State private var visible = false

    var body: some View {
        GeometryReader { g in
            Rectangle()
                .fill(Color.accentColor)
                .frame(width: g.size.width * progress)
        }
        .frame(height: 2.5)
        .opacity(visible ? 1 : 0)
        .onChange(of: active, initial: true) { _, now in
            if now {
                progress = 0
                visible = true
                withAnimation(.easeOut(duration: 8)) { progress = 0.9 }
            } else if visible {
                withAnimation(.easeOut(duration: 0.25)) { progress = 1 }
                withAnimation(.easeIn(duration: 0.3).delay(0.35)) { visible = false }
            }
        }
        .accessibilityHidden(true)
    }
}

/// Press feedback: subtle scale, fast ease-out.
struct PressableStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .animation(.easeOut(duration: 0.16), value: configuration.isPressed)
    }
}

struct InviteBanner: View {
    let seats: [(n: Int, link: String)]
    let isGroup: Bool
    @State private var copiedIdx: Int? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Invitation pending").font(.subheadline).bold()
            Text(isGroup
                ? "One link per member — each is single-use."
                : "Share this link. It carries everything they need.")
                .font(.caption).foregroundStyle(.secondary)
            ForEach(seats, id: \.n) { seat in
                let n = seat.n
                let one = seat.link
                HStack(spacing: 8) {
                    if isGroup {
                        Text("Member \(n + 2)")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .frame(width: 64, alignment: .leading)
                    }
                    ShareLink(item: one) {
                        Label("Share", systemImage: "square.and.arrow.up")
                            .font(.footnote)
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                    Button {
                        UIPasteboard.general.string = one
                        withAnimation(.easeOut(duration: 0.18)) { copiedIdx = n }
                        DispatchQueue.main.asyncAfter(deadline: .now() + 1.4) {
                            withAnimation { copiedIdx = nil }
                        }
                    } label: {
                        Text(copiedIdx == n ? "Copied" : "Copy")
                            .font(.footnote)
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                    .tint(copiedIdx == n ? .green : nil)
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground))
        .sensoryFeedback(.success, trigger: copiedIdx) { _, new in new != nil }
    }
}

/// iOS relocates the app container on every install, so absolute paths
/// stored in the database go stale; re-root them against today's container.
func resolveFile(_ path: String) -> String? {
    if FileManager.default.fileExists(atPath: path) { return path }
    let fm = FileManager.default
    if let r = path.range(of: "/Documents/") {
        let docs = fm.urls(for: .documentDirectory, in: .userDomainMask)[0].path
        let candidate = docs + "/" + String(path[r.upperBound...])
        if fm.fileExists(atPath: candidate) { return candidate }
    }
    if let r = path.range(of: "/.sanctum/") {
        let candidate = NSHomeDirectory() + "/Library/.sanctum/" + String(path[r.upperBound...])
        if fm.fileExists(atPath: candidate) { return candidate }
    }
    return nil
}

func timeHeader(_ d: Date) -> String {
    let time = d.formatted(date: .omitted, time: .shortened)
    if Calendar.current.isDateInToday(d) { return "Today \(time)" }
    if Calendar.current.isDateInYesterday(d) { return "Yesterday \(time)" }
    return d.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day()) + " " + time
}

struct Bubble: View {
    @EnvironmentObject var model: AppModel
    let chatId: Int64
    let msg: MsgVM
    var showSender = true
    /// "Sent" or "Delivered", on the newest finished send only.
    var receipt: String?

    var body: some View {
        HStack {
            if msg.mine { Spacer(minLength: 60) }
            VStack(alignment: msg.mine ? .trailing : .leading, spacing: 2) {
                if showSender, !msg.mine, let seat = msg.senderSeat {
                    Text("Member \(seat + 1)")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                bubbleContent
                    .contextMenu {
                        Text(msg.ts.formatted(date: .abbreviated, time: .shortened))
                        if !msg.mine {
                            // One horizontal row of all six, like iMessage's tapbacks.
                            ControlGroup {
                                ForEach(tapbacks, id: \.self) { emoji in
                                    Button { model.react(chatId, msg, emoji) } label: {
                                        // A palette only draws images; plain emoji text comes out as "?".
                                        Image(uiImage: emojiImage(emoji)).renderingMode(.original)
                                    }
                                    .accessibilityLabel(emoji)
                                }
                            }
                            .controlGroupStyle(.palette)
                        }
                        Button {
                            UIPasteboard.general.string = msg.text
                        } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
                        if msg.mine && !msg.delivered && !msg.failed {
                            Button(role: .destructive) {
                                model.unsend(chatId, msg)
                            } label: {
                                Label("Unsend", systemImage: "arrow.uturn.backward")
                            }
                        }
                        if msg.mine && msg.delivered && chatId >= 0 {
                            Button(role: .destructive) {
                                model.deleteMsg(chatId, msg, everyone: true)
                            } label: {
                                Label("Delete for Everyone", systemImage: "trash.slash")
                            }
                        }
                        Button(role: .destructive) {
                            model.deleteMsg(chatId, msg, everyone: false)
                        } label: {
                            Label("Delete for Me", systemImage: "trash")
                        }
                    }
                ForEach(msg.pay, id: \.raw) { PayChip(link: $0, mine: msg.mine) }
                HStack(spacing: 4) {
                    if let r = msg.reaction { Text(r).font(.caption) }
                    if msg.mine && msg.failed {
                        Label("Not sent", systemImage: "exclamationmark.circle.fill")
                            .font(.caption2)
                            .foregroundStyle(.red)
                    } else if msg.mine && !msg.sent {
                        // This message's own upload progress.
                        SendingBar(active: true).frame(width: 64)
                    } else if let receipt {
                        Text(receipt).font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
            if !msg.mine { Spacer(minLength: 60) }
        }
        .id(msg.id)
    }

    @ViewBuilder
    private var bubbleContent: some View {
        if let card = msg.tao {
            PayCardView(chatId: chatId, wireId: msg.id, mine: msg.mine, card: card)
        } else if let stored = msg.filePath, let path = resolveFile(stored),
           let ui = UIImage(contentsOfFile: path) {
            Image(uiImage: ui)
                .resizable()
                .scaledToFit()
                .frame(maxWidth: 240, maxHeight: 320)
                .clipShape(RoundedRectangle(cornerRadius: 14))
        } else if msg.filePath != nil {
            Label(msg.text, systemImage: "doc")
                .padding(.horizontal, 12)
                .padding(.vertical, 7)
                .background(
                    RoundedRectangle(cornerRadius: 18)
                        .fill(msg.mine ? Color.accentColor : Color(.systemGray5))
                )
                .foregroundStyle(msg.mine ? .white : .primary)
        } else {
            Text(msg.display ?? msg.text)
                .padding(.horizontal, 12)
                .padding(.vertical, 7)
                .background(
                    RoundedRectangle(cornerRadius: 18)
                        .fill(msg.mine ? Color.accentColor : Color(.systemGray5))
                )
                .foregroundStyle(msg.mine ? .white : .primary)
        }
    }
}
