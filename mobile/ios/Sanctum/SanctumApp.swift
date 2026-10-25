import SwiftUI

@main
struct SanctumApp: App {
    @StateObject private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(model)
                .onOpenURL { url in
                    model.join(url.absoluteString)
                }
        }
        // Read receipts only count what's on screen.
        .onChange(of: scenePhase) { _, phase in
            model.isActive = phase == .active
            model.markRead()
        }
    }
}

struct ChatVM: Identifiable {
    let id: Int64
    var name: String
    var waiting: Bool
    var inviteLink: String
    var msgs: [MsgVM]
    var unread: Int

    /// Seat links still unclaimed (claimed group seats arrive as blank lines,
    /// keeping "Member N" labels stable). Empty once everyone has joined.
    var openInvites: [(n: Int, link: String)] {
        inviteLink.split(separator: "\n", omittingEmptySubsequences: false)
            .enumerated()
            .compactMap { n, l in l.isEmpty ? nil : (n, String(l)) }
    }
    var isGroupInvite: Bool { inviteLink.contains("\n") }
}

struct MsgVM: Identifiable {
    let id: String
    let mine: Bool
    var delivered: Bool
    /// Never left this device (only known for this session's sends).
    var failed = false
    /// Upload finished (history is always sent; new sends start false).
    var sent = true
    /// Own message the peer has read (read receipts on).
    var readAt: Date?
    var reaction: String?
    let text: String
    let filePath: String?
    let senderSeat: Int64?
    let ts: Date
}

func toVM(_ m: FfiMsg) -> MsgVM {
    MsgVM(
        id: m.wireId,
        mine: m.mine,
        delivered: m.delivered,
        readAt: m.mine ? m.readMs.map { Date(timeIntervalSince1970: Double($0) / 1000.0) } : nil,
        reaction: m.reaction,
        text: m.text,
        filePath: m.filePath,
        senderSeat: m.senderSeat,
        ts: Date(timeIntervalSince1970: Double(m.tsMs) / 1000.0)
    )
}

extension AppModel {
    var colorScheme: ColorScheme? {
        switch settings.theme {
        case "dark": .dark
        case "light": .light
        default: nil
        }
    }
}

@MainActor
final class AppModel: ObservableObject {
    @Published var chats: [ChatVM] = []
    @Published var selected: Int64?
    /// App is in the foreground (scene active), so an open chat is being seen.
    var isActive = true
    @Published var status = "connecting…"
    @Published var inviting = false
    @Published var hasToken = false
    @Published var settings: FfiSettings
    /// Embedded Tor progress; nil while Tor is off.
    @Published var tor: FfiTorStatus?
    /// Bluetooth mesh, opt-in: advertising tells nearby scanners that a
    /// Sanctum user is around (never who).
    @Published var meshOn = UserDefaults.standard.bool(forKey: "meshOn")
    @Published var meshLinks = 0
    private var mesh: Mesh?

    // Exactly one Client: each spawns a worker, and two workers would race
    // on the same chats' ratchet state.
    private let client: Client
    private var timer: Timer?

    init() {
        let c = Client()
        client = c
        settings = c.getSettings()
        hasToken = client.hasApiToken()
        timer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        if meshOn { setMesh(true) }
    }

    func setMesh(_ on: Bool) {
        meshOn = on
        UserDefaults.standard.set(on, forKey: "meshOn")
        mesh?.stop()
        mesh = on ? Mesh(client: client) { [weak self] n in
            Task { @MainActor in self?.meshLinks = n }
        } : nil
    }

    func tick() {
        let t = torStatus()
        if t != tor { tor = t }
        for ev in client.pollEvents() {
            switch ev {
            case .snapshot(let list):
                chats = list.map { c in
                    ChatVM(
                        id: c.id,
                        name: c.name,
                        waiting: c.waiting,
                        inviteLink: c.inviteLink,
                        msgs: client.snapshotMessages(contactId: c.id).map(toVM),
                        unread: 0
                    )
                }
                status = "ready"
            case .contactAdded(let c):
                inviting = false
                chats.append(ChatVM(
                    id: c.id, name: c.name, waiting: c.waiting,
                    inviteLink: c.inviteLink, msgs: [], unread: 0
                ))
                selected = c.id
            case .contactUpdated(let c):
                if let i = idx(c.id) {
                    chats[i].waiting = c.waiting
                    chats[i].inviteLink = c.inviteLink
                }
            case .contactRemoved(let id):
                chats.removeAll { $0.id == id }
                if selected == id { selected = nil }
            case .peerJoined(let id):
                if let i = idx(id) {
                    chats[i].waiting = false
                    chats[i].inviteLink = ""
                }
            case .received(let id, let msg):
                if let i = idx(id) {
                    chats[i].msgs.append(toVM(msg))
                    if selected != id { chats[i].unread += 1 }
                }
                if selected == id { markRead() }
            case .read(let id, let wireIds, let tsMs):
                if let i = idx(id) {
                    for j in chats[i].msgs.indices where wireIds.contains(chats[i].msgs[j].id) {
                        chats[i].msgs[j].readAt = Date(timeIntervalSince1970: Double(tsMs) / 1000.0)
                        chats[i].msgs[j].delivered = true
                    }
                }
            case .delivered(let id, let wireId):
                if let i = idx(id), let j = chats[i].msgs.firstIndex(where: { $0.id == wireId }) {
                    chats[i].msgs[j].delivered = true
                }
            case .removed(let id, let wireId):
                if let i = idx(id) { chats[i].msgs.removeAll { $0.id == wireId } }
            case .sent(let id, let wireId):
                if let i = idx(id), let j = chats[i].msgs.firstIndex(where: { $0.id == wireId }) {
                    chats[i].msgs[j].sent = true
                }
            case .sendFailed(let id, let wireId, let message):
                if let i = idx(id), let j = chats[i].msgs.firstIndex(where: { $0.id == wireId }) {
                    chats[i].msgs[j].failed = true
                }
                status = "error: not sent: \(message)"
            case .reaction(let id, let refId, let emoji):
                if let i = idx(id), let j = chats[i].msgs.firstIndex(where: { $0.id == refId }) {
                    chats[i].msgs[j].reaction = emoji
                }
            case .history(let id, let msgs):
                if let i = idx(id) { chats[i].msgs = msgs.map(toVM) }
            case .errorEvent(let message):
                inviting = false
                status = "error: \(message)"
            }
        }
    }

    private func idx(_ id: Int64) -> Int? { chats.firstIndex { $0.id == id } }

    func send(_ text: String) {
        guard let sel = selected, let i = idx(sel) else { return }
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        let wireId = client.send(contactId: sel, text: trimmed)
        chats[i].msgs.append(MsgVM(
            id: wireId, mine: true, delivered: false, sent: false, reaction: nil,
            text: trimmed, filePath: nil, senderSeat: nil, ts: Date()
        ))
    }

    func sendFile(_ data: Data, suggestedName: String) {
        guard let sel = selected, let i = idx(sel) else { return }
        // Documents, not tmp: iOS clears tmp between launches, which would
        // break the local preview after a restart.
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = docs.appendingPathComponent("attachments", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let name = suggestedName.isEmpty ? "photo.jpg" : suggestedName
        let url = dir.appendingPathComponent("\(UUID().uuidString)-\(name)")
        do {
            try data.write(to: url)
        } catch { return }
        let wireId = client.sendFile(contactId: sel, path: url.path)
        chats[i].msgs.append(MsgVM(
            id: wireId, mine: true, delivered: false, sent: false, reaction: nil,
            text: name, filePath: url.path, senderSeat: nil, ts: Date()
        ))
    }

    func join(_ link: String) {
        client.acceptInvite(
            link: link.trimmingCharacters(in: .whitespacesAndNewlines),
            name: "Contact \(chats.count + 1)"
        )
    }

    func newInvite() {
        guard !inviting else { return }
        inviting = true
        client.newInvite(name: "Contact \(chats.count + 1)")
    }

    func saveSettings() {
        client.saveSettings(new: settings)
    }

    func setDisappear(_ secs: UInt64) {
        settings.disappearSecs = secs
        saveSettings()
        if let sel = selected {
            client.setTimer(contactId: sel, secs: secs)
        }
    }

    func newGroup(name: String, size: Int64) {
        guard !inviting else { return }
        inviting = true
        client.newGroup(name: name.isEmpty ? "Group" : name, size: size)
    }

    func saveToken(_ token: String) {
        client.setApiToken(token: token)
        hasToken = client.hasApiToken()
    }

    func rename(_ id: Int64, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        client.rename(contactId: id, name: trimmed)
        if let i = idx(id) { chats[i].name = trimmed }
    }

    func deleteChat(_ id: Int64) {
        client.deleteContact(contactId: id)
    }

    func clearHistory(_ id: Int64, olderThanSecs: UInt64?) {
        client.clearHistory(contactId: id, olderThanSecs: olderThanSecs)
    }

    func react(_ chatId: Int64, _ msg: MsgVM, _ emoji: String) {
        client.react(contactId: chatId, refId: msg.id, emoji: emoji)
        if let i = idx(chatId), let j = chats[i].msgs.firstIndex(where: { $0.id == msg.id }) {
            chats[i].msgs[j].reaction = emoji
        }
    }

    /// The chat (default: the selected one) is on screen; the core marks it
    /// read and, with read receipts on, tells the peer.
    func markRead(_ chatId: Int64? = nil) {
        if isActive, let id = chatId ?? selected {
            client.markRead(contactId: id)
        }
    }

    /// Removed from the view when the core confirms (it may refuse).
    func deleteMsg(_ chatId: Int64, _ msg: MsgVM, everyone: Bool) {
        client.deleteMsg(contactId: chatId, wireId: msg.id, everyone: everyone)
    }

    func unsend(_ chatId: Int64, _ msg: MsgVM) {
        client.unsend(contactId: chatId, wireId: msg.id)
        if let i = idx(chatId) {
            chats[i].msgs.removeAll { $0.id == msg.id }
        }
    }
}
