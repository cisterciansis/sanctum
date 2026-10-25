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
    var verified = false
    var blocked = false
    var favorite = false
    /// Someone wrote under a different identity key (refused).
    var keyChanged = false
    /// "12345 67890 …"; empty until the peer joins (and for groups).
    var safety = ""

    init(_ c: FfiContact, msgs: [MsgVM]) {
        id = c.id
        name = c.name
        waiting = c.waiting
        inviteLink = c.inviteLink
        self.msgs = msgs
        unread = 0
        sync(c)
    }

    /// Take the core's view of everything but the name (renames are local).
    mutating func sync(_ c: FfiContact) {
        waiting = c.waiting
        inviteLink = c.inviteLink
        verified = c.verified
        blocked = c.blocked
        favorite = c.favorite
        keyChanged = c.keyChanged
        safety = c.safety
    }

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
    var text: String
    /// TAO payment card, if this message is one.
    var tao: FfiPayCard?
    let filePath: String?
    let senderSeat: Int64?
    let ts: Date
    /// Payment chips, and the text with long payment strings shortened.
    var pay: [PayLink] = []
    var display: String?
}

let bearerWarning = cashuBearerWarning()

func toVM(_ m: FfiMsg) -> MsgVM {
    let scanned = m.filePath == nil ? payScan(text: m.text) : Payments(text: m.text, links: [])
    return MsgVM(
        id: m.wireId,
        mine: m.mine,
        delivered: m.delivered,
        readAt: m.mine ? m.readMs.map { Date(timeIntervalSince1970: Double($0) / 1000.0) } : nil,
        reaction: m.reaction,
        text: m.text,
        tao: m.pay,
        filePath: m.filePath,
        senderSeat: m.senderSeat,
        ts: Date(timeIntervalSince1970: Double(m.tsMs) / 1000.0),
        pay: scanned.links,
        display: scanned.text
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
    /// TAO wallet state, and a recovery phrase the user chose to see.
    @Published var wallet: FfiWallet?
    @Published var phrase: PhraseVM?

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
    }

    func tick() {
        let t = torStatus()
        if t != tor { tor = t }
        for ev in client.pollEvents() {
            switch ev {
            case .snapshot(let list):
                chats = list.map { c in
                    ChatVM(c, msgs: client.snapshotMessages(contactId: c.id).map(toVM))
                }
                status = "ready"
            case .contactAdded(let c):
                inviting = false
                chats.append(ChatVM(c, msgs: []))
                selected = c.id
            case .contactUpdated(let c):
                if let i = idx(c.id) { chats[i].sync(c) }
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
            case .msgUpdated(let id, let wireId, let text, let pay):
                if let i = idx(id), let j = chats[i].msgs.firstIndex(where: { $0.id == wireId }) {
                    chats[i].msgs[j].text = text
                    chats[i].msgs[j].tao = pay
                }
            case .taoWallet(let w):
                wallet = w
            case .taoPhrase(let address, let words):
                phrase = PhraseVM(address: address, words: words.split(separator: " ").map(String.init))
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

    /// The chat list: favorites first, otherwise in order.
    var listed: [ChatVM] { chats.filter(\.favorite) + chats.filter { !$0.favorite } }

    /// Returns why it wasn't sent (a bad `/pay` token), if it wasn't.
    func send(_ text: String) -> String? {
        guard let sel = selected, let i = idx(sel) else { return nil }
        var body = text.trimmingCharacters(in: .whitespaces)
        guard !body.isEmpty else { return nil }
        if body.hasPrefix("/pay ") {
            guard let token = payToken(input: String(body.dropFirst(5))) else {
                return "That isn't a valid Cashu token (cashuA… or cashuB…)."
            }
            body = token
        }
        let wireId = client.send(contactId: sel, text: body)
        let scanned = payScan(text: body)
        chats[i].msgs.append(MsgVM(
            id: wireId, mine: true, delivered: false, sent: false, reaction: nil,
            text: body, filePath: nil, senderSeat: nil, ts: Date(),
            pay: scanned.links, display: scanned.text
        ))
        return nil
    }

    // Shown at once; the core's ContactUpdated confirms a moment later.
    func setVerified(_ id: Int64, _ on: Bool) {
        client.setVerified(contactId: id, on: on)
        if let i = idx(id) {
            chats[i].verified = on
            if on { chats[i].keyChanged = false }
        }
    }

    func setBlocked(_ id: Int64, _ on: Bool) {
        client.setBlocked(contactId: id, on: on)
        if let i = idx(id) { chats[i].blocked = on }
    }

    func setFavorite(_ id: Int64, _ on: Bool) {
        client.setFavorite(contactId: id, on: on)
        if let i = idx(id) { chats[i].favorite = on }
    }

    /// Emergency wipe (triple-tap the title): database, keychain key,
    /// settings, ratchets and files all go, at once and offline, and the
    /// app is back at first launch.
    func wipe() {
        let failed = client.wipe()
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        try? FileManager.default.removeItem(at: docs.appendingPathComponent("attachments"))
        chats = []
        selected = nil
        inviting = false
        settings = client.getSettings()
        hasToken = client.hasApiToken()
        status = failed.map { "error: wipe incomplete: \($0)" } ?? "connecting…"
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

    // TAO wallet; payments confirm in the UI before these are called.
    func taoRefresh() { client.taoRefresh() }
    func taoNew() { client.taoNew(name: "Account \((wallet?.accounts.count ?? 0) + 1)") }
    func taoImport(_ phrase: String) { client.taoImport(name: "Imported", phrase: phrase) }
    func taoWatch(_ address: String) { client.taoWatch(name: "Watch-only", address: address) }
    func taoUse(_ address: String) { client.taoUse(address: address) }
    func taoForget(_ address: String) { client.taoForget(address: address) }
    func taoReveal(_ address: String) { client.taoReveal(address: address) }
    func taoSetTestnet(_ on: Bool) { client.taoSetTestnet(testnet: on) }
    func taoShare(_ chatId: Int64) { client.taoShare(contactId: chatId) }
    func taoPay(_ chatId: Int64, amount: String, to: String?, re: String?) -> Bool {
        client.taoPay(contactId: chatId, amount: amount, to: to, re: re)
    }
    func taoRequest(_ chatId: Int64, amount: String, memo: String?) -> Bool {
        client.taoRequest(contactId: chatId, amount: amount, memo: memo)
    }
    func taoCheque(_ chatId: Int64, amount: String) -> Bool {
        client.taoCheque(contactId: chatId, amount: amount)
    }
    func taoCash(_ chatId: Int64, wireId: String) { client.taoCash(contactId: chatId, wireId: wireId) }

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
