import SwiftUI
import PhotosUI

let tapbacks = ["👍", "👎", "❤️", "‼️", "❓", "😂"]

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    @State private var showJoin = false
    @State private var showSettings = false
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
                                Button {
                                    UIPasteboard.general.string = chat.inviteLink
                                } label: {
                                    Label("Copy Invite Link", systemImage: "link")
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
                    .disabled(!link.trimmingCharacters(in: .whitespacesAndNewlines)
                        .hasPrefix("sanctum://v2/"))
                } footer: {
                    Text("An invite carries everything you need — no account, no signup.")
                }
            }
            .navigationTitle("Join a chat")
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
                PhotosPicker(selection: $photoItem, matching: .images) {
                    Image(systemName: "plus")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 30, height: 30)
                        .background(Circle().fill(Color(.systemGray5)))
                }
                .buttonStyle(PressableStyle())
                .disabled(chat.waiting)
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
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Invitation pending").font(.subheadline).bold()
            Text("Share this link. It carries everything they need.")
                .font(.caption).foregroundStyle(.secondary)
            HStack(spacing: 8) {
                ShareLink(item: link) {
                    Label("Share", systemImage: "square.and.arrow.up")
                        .font(.footnote)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                Button {
                    UIPasteboard.general.string = link
                    withAnimation(.easeOut(duration: 0.18)) { copied = true }
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.4) {
                        withAnimation { copied = false }
                    }
                } label: {
                    Text(copied ? "Copied" : "Copy")
                        .font(.footnote)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .tint(copied ? .green : nil)
                .sensoryFeedback(.success, trigger: copied) { _, new in new }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground))
    }
}

struct Bubble: View {
    @EnvironmentObject var model: AppModel
    let chatId: Int64
    let msg: MsgVM

    var body: some View {
        HStack {
            if msg.mine { Spacer(minLength: 60) }
            VStack(alignment: msg.mine ? .trailing : .leading, spacing: 2) {
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
        if let path = msg.filePath, let ui = UIImage(contentsOfFile: path) {
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
