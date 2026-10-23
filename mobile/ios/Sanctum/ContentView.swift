import SwiftUI

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    @State private var showJoin = false

    var body: some View {
        NavigationSplitView {
            List(selection: $model.selected) {
                ForEach(model.chats) { chat in
                    HStack {
                        Text(chat.name)
                        Spacer()
                        if chat.waiting {
                            Circle().fill(.secondary).frame(width: 7, height: 7)
                        } else if chat.unread > 0 {
                            Text("\(chat.unread)")
                                .font(.caption2).bold()
                                .padding(.horizontal, 6).padding(.vertical, 2)
                                .background(Capsule().fill(Color.accentColor))
                                .foregroundStyle(.white)
                        }
                    }
                    .tag(chat.id)
                }
            }
            .navigationTitle("Chats")
            .safeAreaInset(edge: .bottom) {
                if model.status.hasPrefix("error") || model.status == "connecting…" {
                    Text(model.status)
                        .font(.caption2)
                        .foregroundStyle(model.status.hasPrefix("error") ? .red : .secondary)
                        .frame(maxWidth: .infinity)
                        .padding(6)
                        .background(.bar)
                }
            }
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button { showJoin = true } label: { Image(systemName: "plus") }
                }
            }
            .overlay {
                if model.chats.isEmpty {
                    ContentUnavailableView(
                        "No chats yet",
                        systemImage: "lock.shield",
                        description: Text("Paste an invite link to join a conversation.")
                    )
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
                    .disabled(!link.hasPrefix("sanctum://v2/"))
                } footer: {
                    Text("An invite carries everything you need — no account, no signup.")
                }
            }
            .navigationTitle("Join a chat")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

struct ChatView: View {
    @EnvironmentObject var model: AppModel
    let chat: ChatVM
    @State private var draft = ""

    var body: some View {
        VStack(spacing: 0) {
            if chat.waiting && !chat.inviteLink.isEmpty {
                InviteBanner(link: chat.inviteLink)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 4) {
                        ForEach(chat.msgs) { msg in
                            Bubble(msg: msg)
                        }
                    }
                    .padding(12)
                }
                .onChange(of: chat.msgs.count) {
                    if let last = chat.msgs.last {
                        withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
            HStack(spacing: 8) {
                TextField("", text: $draft, axis: .vertical)
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
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 15, weight: .bold))
                        .foregroundStyle(.white)
                        .frame(width: 32, height: 32)
                        .background(Circle().fill(Color.accentColor))
                }
                .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty || chat.waiting)
            }
            .padding(10)
        }
        .navigationTitle(chat.name)
        .navigationBarTitleDisplayMode(.inline)
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
            Button {
                UIPasteboard.general.string = link
                withAnimation { copied = true }
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.4) {
                    withAnimation { copied = false }
                }
            } label: {
                Text(copied ? "Copied" : "Copy invite link")
                    .font(.footnote)
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground))
    }
}

struct Bubble: View {
    let msg: MsgVM

    var body: some View {
        HStack {
            if msg.mine { Spacer(minLength: 60) }
            VStack(alignment: msg.mine ? .trailing : .leading, spacing: 2) {
                Text(msg.text)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 7)
                    .background(
                        RoundedRectangle(cornerRadius: 18)
                            .fill(msg.mine ? Color.accentColor : Color(.systemGray5))
                    )
                    .foregroundStyle(msg.mine ? .white : .primary)
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
}
