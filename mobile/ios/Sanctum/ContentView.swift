import SwiftUI
import PhotosUI
import UniformTypeIdentifiers

let tapbacks = ["👍", "👎", "❤️", "‼️", "❓", "😂"]

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    @State private var showJoin = false
    @State private var showSettings = false
    @State private var showNewGroup = false
    @State private var renaming: ChatVM?
    @State private var renameText = ""

    var body: some View {
        NavigationSplitView {
            List(selection: $model.selected) {
                ForEach(model.chats) { chat in
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
                            if chat.waiting && !chat.inviteLink.isEmpty {
                                let links = chat.inviteLink.split(separator: "\n").map(String.init)
                                if links.count == 1 {
                                    Button {
                                        UIPasteboard.general.string = links[0]
                                    } label: {
                                        Label("Copy Invite Link", systemImage: "link")
                                    }
                                } else {
                                    Menu {
                                        ForEach(Array(links.enumerated()), id: \.offset) { n, one in
                                            Button("Member \(n + 2)") {
                                                UIPasteboard.general.string = one
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
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { showSettings = true } label: {
                        Image(systemName: "gearshape")
                    }
                }
                ToolbarItem(placement: .primaryAction) {
                    Menu {
                        Button {
                            if model.hasToken {
                                model.newInvite()
                            } else {
                                showSettings = true
                            }
                        } label: {
                            Label("New Invite", systemImage: "person.badge.plus")
                        }
                        Button {
                            if model.hasToken {
                                showNewGroup = true
                            } else {
                                showSettings = true
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
                Text(chat.name)
                if let last = chat.msgs.last {
                    Text(last.text)
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
            if chat.waiting {
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

    var body: some View {
        NavigationStack {
            Form {
                Section("Invite link") {
                    TextField("sanctum://v2/…", text: $link, axis: .vertical)
                        .lineLimit(3...6)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
                Section {
                    Button("Join") {
                        model.join(link)
                        dismiss()
                    }
                    .disabled({
                        let t = link.trimmingCharacters(in: .whitespacesAndNewlines)
                        return !(t.hasPrefix("sanctum://v2/") || t.hasPrefix("sanctum://g1/"))
                    }())
                } footer: {
                    Text("An invite carries everything you need — no account, no signup.")
                }
            }
            .navigationTitle("Join a chat")
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

struct SettingsSheet: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) var dismiss
    @State private var token = ""

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
                    Toggle("Route via tunnel (Tor / UR)", isOn: Binding(
                        get: { model.settings.proxyEnabled },
                        set: { model.settings.proxyEnabled = $0; model.saveSettings() }
                    ))
                } header: {
                    Text("Messages & network")
                } footer: {
                    Text("Tunnel: \(model.settings.proxyUrl) · applies on next launch")
                }
                Section {
                    if model.hasToken {
                        Label("Inviting enabled", systemImage: "checkmark.seal.fill")
                            .foregroundStyle(.green)
                    }
                    SecureField("Hippius API token", text: $token)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                    Button(model.hasToken ? "Replace token" : "Save token") {
                        model.saveToken(token)
                        token = ""
                    }
                    .disabled(token.trimmingCharacters(in: .whitespaces).isEmpty)
                } header: {
                    Text("Creating invites")
                } footer: {
                    Text("Only inviting needs a token (console.hippius.com → Settings). Joining never does. The token stays on this device.")
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
    @State private var showRename = false
    @State private var renameText = ""

    var body: some View {
        VStack(spacing: 0) {
            if chat.waiting && !chat.inviteLink.isEmpty {
                InviteBanner(link: chat.inviteLink)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 4) {
                        ForEach(chat.msgs) { msg in
                            Bubble(chatId: chat.id, msg: msg)
                                .transition(.scale(scale: 0.96).combined(with: .opacity))
                        }
                    }
                    .padding(12)
                }
                .scrollDismissesKeyboard(.interactively)
                .animation(.spring(duration: 0.25, bounce: 0.15), value: chat.msgs.count)
                .onChange(of: chat.msgs.count) {
                    if let last = chat.msgs.last {
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
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 30, height: 30)
                        .background(Circle().fill(Color(.systemGray5)))
                }
                .disabled(chat.waiting)
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
                    model.send(draft)
                    draft = ""
                    sendPulse += 1
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
                    if chat.waiting && !chat.inviteLink.isEmpty {
                        let links = chat.inviteLink.split(separator: "\n").map(String.init)
                        if links.count == 1 {
                            Button {
                                UIPasteboard.general.string = links[0]
                            } label: {
                                Label("Copy Invite Link", systemImage: "link")
                            }
                            ShareLink(item: links[0]) {
                                Label("Share Invite", systemImage: "square.and.arrow.up")
                            }
                        } else {
                            Menu {
                                ForEach(Array(links.enumerated()), id: \.offset) { n, one in
                                    Button("Member \(n + 2)") {
                                        UIPasteboard.general.string = one
                                    }
                                }
                            } label: {
                                Label("Copy Invite…", systemImage: "link")
                            }
                        }
                    }
                    if chat.id >= 0 {
                        Menu {
                            ForEach([(UInt64(0), "Off"), (60, "1 minute"), (3600, "1 hour"), (86400, "1 day")], id: \.0) { secs, label in
                                Button {
                                    model.setDisappear(secs)
                                } label: {
                                    if model.settings.disappearSecs == secs {
                                        Label(label, systemImage: "checkmark")
                                    } else {
                                        Text(label)
                                    }
                                }
                            }
                        } label: {
                            Label("Disappearing Messages", systemImage: "timer")
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
    let link: String
    @State private var copiedIdx: Int? = nil

    var body: some View {
        let links = link.split(separator: "\n").map(String.init)
        VStack(alignment: .leading, spacing: 6) {
            Text("Invitation pending").font(.subheadline).bold()
            Text(links.count > 1
                ? "One link per member — each is single-use."
                : "Share this link. It carries everything they need.")
                .font(.caption).foregroundStyle(.secondary)
            ForEach(Array(links.enumerated()), id: \.offset) { n, one in
                HStack(spacing: 8) {
                    if links.count > 1 {
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
        let candidate = NSHomeDirectory() + "/.sanctum/" + String(path[r.upperBound...])
        if fm.fileExists(atPath: candidate) { return candidate }
    }
    return nil
}

struct Bubble: View {
    @EnvironmentObject var model: AppModel
    let chatId: Int64
    let msg: MsgVM

    var body: some View {
        HStack {
            if msg.mine { Spacer(minLength: 60) }
            VStack(alignment: msg.mine ? .trailing : .leading, spacing: 2) {
                if !msg.mine, let seat = msg.senderSeat {
                    Text("Member \(seat + 1)")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                bubbleContent
                    .contextMenu {
                        if !msg.mine {
                            ControlGroup {
                                ForEach(tapbacks.prefix(4), id: \.self) { emoji in
                                    Button(emoji) { model.react(chatId, msg, emoji) }
                                }
                            }
                            ForEach(tapbacks.suffix(2), id: \.self) { emoji in
                                Button(emoji) { model.react(chatId, msg, emoji) }
                            }
                        }
                        Button {
                            UIPasteboard.general.string = msg.text
                        } label: {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
                        if msg.mine && !msg.delivered {
                            Button(role: .destructive) {
                                model.unsend(chatId, msg)
                            } label: {
                                Label("Unsend", systemImage: "arrow.uturn.backward")
                            }
                        }
                    }
                HStack(spacing: 4) {
                    if let r = msg.reaction { Text(r).font(.caption) }
                    if model.settings.timestamps {
                        Text(msg.ts, style: .time).font(.caption2).foregroundStyle(.secondary)
                    }
                    if msg.mine && msg.delivered {
                        Text("Delivered").font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
            if !msg.mine { Spacer(minLength: 60) }
        }
        .id(msg.id)
    }

    @ViewBuilder
    private var bubbleContent: some View {
        if let stored = msg.filePath, let path = resolveFile(stored),
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
            Text(msg.text)
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
