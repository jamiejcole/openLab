// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
import Foundation

/// A network interface of this Mac that is up: the system's name for it (`en0`) and its IPv4 address.
struct NetworkInterface: Equatable {
    var name: String
    var address: String
}

/// One entry of a network patch's interface pop-up: what it shows and what it stores (nil: the
/// system routes the message).
struct NetworkInterfaceChoice: Equatable {
    var title: String
    var value: String?
}

enum NetworkInterfaces {
    /// The interfaces that are up and have an IPv4 address, loopback included (a patch may send to
    /// this Mac), in the system's order, each name once.
    static func current() -> [NetworkInterface] {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return [] }
        defer { freeifaddrs(list) }
        var found: [NetworkInterface] = []
        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let entry = cursor {
            defer { cursor = entry.pointee.ifa_next }
            let flags = Int32(entry.pointee.ifa_flags)
            guard flags & IFF_UP != 0, let addr = entry.pointee.ifa_addr, addr.pointee.sa_family == UInt8(AF_INET)
            else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard
                getnameinfo(
                    addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0
            else { continue }
            let name = String(cString: entry.pointee.ifa_name)
            if !found.contains(where: { $0.name == name }) {
                found.append(NetworkInterface(name: name, address: String(cString: host)))
            }
        }
        return found
    }

    /// The pop-up's entries: Automatic, then each interface, then the stored one if this Mac lacks
    /// it (a show moved from another Mac keeps its choice, marked, until it is changed).
    static func choices(stored: String?, available: [NetworkInterface]) -> [NetworkInterfaceChoice] {
        var out = [NetworkInterfaceChoice(title: loc("settings.network.interfaceAutomatic"), value: nil)]
        for i in available {
            out.append(
                NetworkInterfaceChoice(
                    title: String(format: loc("settings.network.interfaceItem"), i.name, i.address), value: i.name))
        }
        if let stored, !available.contains(where: { $0.name == stored }) {
            out.append(
                NetworkInterfaceChoice(
                    title: String(format: loc("settings.network.interfaceAbsent"), stored), value: stored))
        }
        return out
    }
}
