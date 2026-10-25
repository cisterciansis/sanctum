import CoreBluetooth
import Foundation

/// Offline carrier over Bluetooth LE, after bitchat's BLEService: every
/// device is both central and peripheral on one service with one
/// characteristic, and floods the packets the core hands it. The core does
/// framing, dedup, TTL, rate limits and all crypto; this only moves frames.
/// Links are keyed by CoreBluetooth's per-device identifiers, which never
/// leave this phone.
final class Mesh: NSObject {
    static let service = CBUUID(string: "5890BC6C-A0B0-475E-8860-61825E2E33C5")
    static let characteristic = CBUUID(string: "62167AA8-21AC-4594-BCE8-4266CD6EBE15")
    /// bitchat's central-link budget; peripheral-side links are uncapped
    /// (CoreBluetooth limits them) and the core rate-limits each one.
    private static let maxCentralLinks = 6
    /// Frames queued per link before new packets are dropped (~256 KB).
    private static let maxQueued = 512

    private let client: Client
    private let onLinks: (Int) -> Void
    private let queue = DispatchQueue(label: "chat.sanctum.mesh")
    private var central: CBCentralManager!
    private var peripheral: CBPeripheralManager!
    private var local: CBMutableCharacteristic?

    // Central role: peripherals we connect to (retained while connecting).
    private var peripherals: [UUID: CBPeripheral] = [:]
    private var remote: [UUID: CBCharacteristic] = [:]
    // Peripheral role: centrals subscribed to us.
    private var centrals: [UUID: CBCentral] = [:]
    private var queued: [UUID: [Data]] = [:]
    private var timer: DispatchSourceTimer?
    private var ticks = 0

    init(client: Client, onLinks: @escaping (Int) -> Void) {
        self.client = client
        self.onLinks = onLinks
        super.init()
        central = CBCentralManager(delegate: self, queue: queue)
        peripheral = CBPeripheralManager(delegate: self, queue: queue)
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 0.25, repeating: 0.25)
        t.setEventHandler { [weak self] in self?.tick() }
        t.resume()
        timer = t
    }

    func stop() {
        queue.sync {
            timer?.cancel()
            central.stopScan()
            peripherals.values.forEach { central.cancelPeripheralConnection($0) }
            peripheral.stopAdvertising()
            peripheral.removeAllServices()
            peripherals = [:]; remote = [:]; centrals = [:]; queued = [:]
            linksChanged()
        }
    }

    private var links: Int { remote.count + centrals.count }

    private func linksChanged() {
        meshSetLinks(count: UInt32(links))
        onLinks(links)
    }

    /// Own packets out every 250 ms; scan duty cycle once a second.
    private func tick() {
        for p in meshOutbox() { broadcast(p, except: nil) }
        ticks += 1
        if ticks % 4 == 0 { dutyCycle() }
    }

    /// bitchat's scan policy: scan continuously with few links, otherwise
    /// 5 s on / 10 s off to save battery.
    private func dutyCycle() {
        guard central.state == .poweredOn else { return }
        let off = links > 2 && (ticks / 4) % 15 >= 5
        if off, central.isScanning {
            central.stopScan()
        } else if !off, !central.isScanning {
            central.scanForPeripherals(withServices: [Self.service])
        }
    }

    private func broadcast(_ packet: Data, except: UUID?) {
        for id in Array(remote.keys) + Array(centrals.keys) where id != except {
            let mtu = remote[id] != nil
                ? peripherals[id]?.maximumWriteValueLength(for: .withoutResponse) ?? 20
                : centrals[id]?.maximumUpdateValueLength ?? 20
            // A packet's frames stay contiguous on a link: all or none.
            let frames = meshFrames(packet: packet, mtu: UInt32(mtu))
            guard (queued[id]?.count ?? 0) + frames.count <= Self.maxQueued else { continue }
            queued[id, default: []] += frames
            pump(id)
        }
    }

    private func pump(_ id: UUID) {
        while let frame = queued[id]?.first {
            if let p = peripherals[id], let c = remote[id] {
                guard p.canSendWriteWithoutResponse else { return }
                p.writeValue(frame, for: c, type: .withoutResponse)
            } else if let c = centrals[id], let local {
                guard peripheral.updateValue(frame, for: local, onSubscribedCentrals: [c]) else { return }
            } else {
                queued[id] = nil
                return
            }
            queued[id]?.removeFirst()
        }
    }

    private func received(_ frame: Data, from id: UUID) {
        guard let relay = client.meshReceive(link: id.uuidString, frame: frame) else { return }
        // Jitter desynchronizes neighbors relaying the same packet.
        queue.asyncAfter(deadline: .now() + .milliseconds(Int.random(in: 10...60))) { [weak self] in
            self?.broadcast(relay, except: id)
        }
    }

    private func drop(_ id: UUID) {
        // Without duplicate reports a scan never re-reports a peripheral it
        // has seen, so rescan to find it again.
        if peripherals[id] != nil, central.state == .poweredOn {
            central.stopScan()
            central.scanForPeripherals(withServices: [Self.service])
        }
        peripherals[id] = nil
        remote[id] = nil
        centrals[id] = nil
        queued[id] = nil
        meshLinkClosed(link: id.uuidString)
        linksChanged()
    }
}

extension Mesh: CBCentralManagerDelegate, CBPeripheralDelegate {
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state == .poweredOn {
            central.scanForPeripherals(withServices: [Self.service])
        } else {
            peripherals.keys.forEach(drop)
        }
    }

    func centralManager(
        _ central: CBCentralManager, didDiscover p: CBPeripheral,
        advertisementData: [String: Any], rssi RSSI: NSNumber
    ) {
        guard peripherals[p.identifier] == nil, peripherals.count < Self.maxCentralLinks else { return }
        peripherals[p.identifier] = p
        p.delegate = self
        central.connect(p)
        // CoreBluetooth never times out a connect; free the slot ourselves.
        queue.asyncAfter(deadline: .now() + 10) { [weak self] in
            guard let self, self.peripherals[p.identifier] != nil, self.remote[p.identifier] == nil else { return }
            central.cancelPeripheralConnection(p)
            self.drop(p.identifier)
        }
    }

    func centralManager(_ central: CBCentralManager, didConnect p: CBPeripheral) {
        p.discoverServices([Self.service])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect p: CBPeripheral, error: Error?) {
        drop(p.identifier)
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral p: CBPeripheral, error: Error?) {
        drop(p.identifier)
    }

    func peripheral(_ p: CBPeripheral, didDiscoverServices error: Error?) {
        guard let s = p.services?.first(where: { $0.uuid == Self.service }) else {
            return central.cancelPeripheralConnection(p)
        }
        p.discoverCharacteristics([Self.characteristic], for: s)
    }

    func peripheral(_ p: CBPeripheral, didDiscoverCharacteristicsFor s: CBService, error: Error?) {
        guard let c = s.characteristics?.first(where: { $0.uuid == Self.characteristic }) else {
            return central.cancelPeripheralConnection(p)
        }
        remote[p.identifier] = c
        p.setNotifyValue(true, for: c)
        linksChanged()
    }

    func peripheral(_ p: CBPeripheral, didUpdateValueFor c: CBCharacteristic, error: Error?) {
        if let v = c.value { received(v, from: p.identifier) }
    }

    func peripheralIsReady(toSendWriteWithoutResponse p: CBPeripheral) {
        pump(p.identifier)
    }
}

extension Mesh: CBPeripheralManagerDelegate {
    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        guard peripheral.state == .poweredOn else {
            centrals.keys.forEach(drop)
            return
        }
        peripheral.removeAllServices()
        let c = CBMutableCharacteristic(
            type: Self.characteristic, properties: [.writeWithoutResponse, .notify],
            value: nil, permissions: [.writeable]
        )
        let s = CBMutableService(type: Self.service, primary: true)
        s.characteristics = [c]
        local = c
        peripheral.add(s)
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, didAdd service: CBService, error: Error?) {
        // Service UUID only: no name, nothing that identifies the user.
        if error == nil {
            peripheral.startAdvertising([CBAdvertisementDataServiceUUIDsKey: [Self.service]])
        }
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, central c: CBCentral, didSubscribeTo _: CBCharacteristic) {
        centrals[c.identifier] = c
        linksChanged()
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, central c: CBCentral, didUnsubscribeFrom _: CBCharacteristic) {
        drop(c.identifier)
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveWrite requests: [CBATTRequest]) {
        for r in requests {
            if let v = r.value { received(v, from: r.central.identifier) }
        }
    }

    func peripheralManagerIsReady(toUpdateSubscribers peripheral: CBPeripheralManager) {
        centrals.keys.forEach(pump)
    }
}
