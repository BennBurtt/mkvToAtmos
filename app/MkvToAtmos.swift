// MkvToAtmos: glisser un MKV (Dolby TrueHD Atmos), récupérer un ADM BWF et un WAV 7.1.4.
// Interface SwiftUI autour de l'outil en ligne de commande `atmosconv` (et de `truehdd`),
// tous deux embarqués dans Contents/Resources.

import AppKit
import SwiftUI
import UniformTypeIdentifiers

// MARK: - Modèle

enum JobState: Equatable {
    case waiting, running, done, failed(String)
}

final class Job: ObservableObject, Identifiable {
    let id = UUID()
    let input: URL
    @Published var state: JobState = .waiting
    @Published var step: String = "En attente"
    @Published var progress: Double = 0
    @Published var outputs: [URL] = []
    @Published var notes: [String] = []
    @Published var overall: Double = 0
    @Published var remaining: String = ""
    @Published var log: [String] = []
    var outputDir: URL?
    var startedAt: Date?
    private var smoothedTotal: Double?

    func addLog(_ line: String) {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        log.append("\(f.string(from: Date()))  \(line)")
    }

    /// Updates the remaining-time estimate from overall progress (smoothed).
    func updateEstimate() {
        guard let start = startedAt, overall > 0.01 else { remaining = "estimation en cours…"; return }
        let elapsed = Date().timeIntervalSince(start)
        guard elapsed > 3 else { remaining = "estimation en cours…"; return }
        let total = elapsed / overall
        smoothedTotal = smoothedTotal.map { $0 * 0.85 + total * 0.15 } ?? total
        let left = max(0, (smoothedTotal ?? total) - elapsed)
        remaining = "environ " + Job.duration(left) + " restantes"
    }

    static func duration(_ t: TimeInterval) -> String {
        let s = Int(t.rounded())
        if s < 60 { return "\(max(s, 1)) s" }
        if s < 3600 { return "\(s / 60) min \(String(format: "%02d", s % 60)) s" }
        return "\(s / 3600) h \(String(format: "%02d", (s % 3600) / 60)) min"
    }

    func saveLog() {
        guard let dir = outputDir else { return }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let text = log.joined(separator: "\n") + "\n"
        try? text.write(to: dir.appendingPathComponent("journal.txt"), atomically: true, encoding: .utf8)
    }

    init(input: URL) { self.input = input }
}

final class Converter: ObservableObject {
    @Published var jobs: [Job] = []
    @Published var busy = false

    @Published var splitChapters = Converter.pref("splitChapters", true) { didSet { save("splitChapters", splitChapters) } }
    @Published var makeADM = Converter.pref("makeADM", true) { didSet { save("makeADM", makeADM) } }
    @Published var make714 = Converter.pref("make714", true) { didSet { save("make714", make714) } }
    @Published var keepDAMF = Converter.pref("keepDAMF", false) { didSet { save("keepDAMF", keepDAMF) } }

    private static func pref(_ key: String, _ def: Bool) -> Bool {
        UserDefaults.standard.object(forKey: key) as? Bool ?? def
    }
    private func save(_ key: String, _ v: Bool) { UserDefaults.standard.set(v, forKey: key) }

    private var process: Process?

    func add(_ urls: [URL]) {
        for u in urls where u.pathExtension.lowercased() == "mkv" || u.pathExtension.lowercased() == "mka" {
            if !jobs.contains(where: { $0.input == u && $0.state == .waiting }) {
                jobs.append(Job(input: u))
            }
        }
        runNext()
    }

    func clearFinished() {
        jobs.removeAll { $0.state != .waiting && $0.state != .running }
    }

    func cancel() {
        process?.terminate()
    }

    private func resource(_ name: String) -> String? {
        if let p = Bundle.main.path(forResource: name, ofType: nil) { return p }
        let local = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent().appendingPathComponent(name).path
        return FileManager.default.isExecutableFile(atPath: local) ? local : nil
    }

    private func runNext() {
        guard !busy, let job = jobs.first(where: { $0.state == .waiting }) else { return }
        guard let atmosconv = resource("atmosconv"), let truehdd = resource("truehdd") else {
            job.state = .failed("Outils internes introuvables dans l'application.")
            return
        }
        busy = true
        job.state = .running
        job.step = "Démarrage"
        job.startedAt = Date()
        job.addLog("Fichier : \(job.input.path)")

        let outDir = job.input.deletingLastPathComponent()
            .appendingPathComponent(job.input.deletingPathExtension().lastPathComponent + " - Atmos")
        job.outputDir = outDir

        var args = ["convert", job.input.path, "--out", outDir.path, "--truehdd", truehdd, "--json"]
        if splitChapters { args.append("--chapters") }
        if !makeADM { args.append("--no-adm") }
        if !make714 { args.append("--no-714") }
        if keepDAMF { args.append("--keep-damf") }

        let p = Process()
        p.executableURL = URL(fileURLWithPath: atmosconv)
        p.arguments = args
        job.addLog("Options : " + args.dropFirst(2).filter { $0.hasPrefix("--") && $0 != "--json" && $0 != "--truehdd" && $0 != "--out" }.joined(separator: " "))
        let out = Pipe()
        let err = Pipe()
        p.standardOutput = out
        p.standardError = err
        process = p
        err.fileHandleForReading.readabilityHandler = { h in
            let d = h.availableData
            if d.isEmpty { return }
            if let t = String(data: d, encoding: .utf8) {
                DispatchQueue.main.async {
                    for l in t.split(separator: "\n") where !l.trimmingCharacters(in: .whitespaces).isEmpty {
                        job.addLog("[stderr] " + l)
                    }
                }
            }
        }
        // Keeps the remaining time moving between progress messages.
        let ticker = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            if job.state == .running { job.updateEstimate() }
        }

        var buffer = Data()
        out.fileHandleForReading.readabilityHandler = { [weak self] h in
            let chunk = h.availableData
            if chunk.isEmpty { return }
            buffer.append(chunk)
            while let nl = buffer.firstIndex(of: 0x0A) {
                let line = buffer.subdata(in: buffer.startIndex..<nl)
                buffer.removeSubrange(buffer.startIndex...nl)
                if let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any] {
                    DispatchQueue.main.async { self?.handle(obj, job: job) }
                }
            }
        }
        p.terminationHandler = { [weak self] proc in
            out.fileHandleForReading.readabilityHandler = nil
            err.fileHandleForReading.readabilityHandler = nil
            DispatchQueue.main.async {
                ticker.invalidate()
                guard let self else { return }
                if case .failed = job.state {
                } else if proc.terminationReason == .uncaughtSignal {
                    job.state = .failed("Conversion annulée.")
                } else if proc.terminationStatus == 0 {
                    job.state = .done
                    job.step = "Terminé"
                    job.progress = 1
                    job.overall = 1
                } else {
                    job.state = .failed("La conversion s'est arrêtée (code \(proc.terminationStatus)). Le journal est enregistré dans le dossier de sortie.")
                }
                let took = Job.duration(Date().timeIntervalSince(job.startedAt ?? Date()))
                switch job.state {
                case .done: job.addLog("Terminé en \(took)")
                case .failed(let m): job.addLog("ÉCHEC : \(m)")
                default: break
                }
                job.saveLog()
                self.busy = false
                self.process = nil
                self.runNext()
            }
        }
        do {
            try p.run()
        } catch {
            ticker.invalidate()
            job.state = .failed("Impossible de lancer la conversion : \(error.localizedDescription)")
            job.addLog("ÉCHEC : \(error.localizedDescription)")
            busy = false
            process = nil
            runNext()
        }
    }

    private func handle(_ obj: [String: Any], job: Job) {
        switch obj["type"] as? String {
        case "progress":
            job.step = obj["label"] as? String ?? job.step
            job.progress = obj["value"] as? Double ?? job.progress
            if let o = obj["overall"] as? Double { job.overall = max(job.overall, o) }
            job.updateEstimate()
        case "step":
            if let m = obj["message"] as? String { job.addLog("▶︎ " + m) }
        case "info":
            if let m = obj["message"] as? String {
                job.addLog(m)
                if m.hasPrefix("attention") || m.contains("objets") { job.notes.append(m) }
            }
        case "file":
            if let p = obj["path"] as? String {
                job.outputs.append(URL(fileURLWithPath: p))
                job.addLog("Fichier créé : " + URL(fileURLWithPath: p).lastPathComponent)
            }
        case "error":
            let m = obj["message"] as? String ?? "Erreur inconnue"
            job.state = .failed(m)
            job.addLog("ERREUR : " + m)
        default:
            break
        }
    }
}

// MARK: - Vues

struct DropZone: View {
    @ObservedObject var conv: Converter
    @State private var hovering = false

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 14)
                .strokeBorder(style: StrokeStyle(lineWidth: 2, dash: [8, 6]))
                .foregroundColor(hovering ? .accentColor : .secondary.opacity(0.6))
                .background(RoundedRectangle(cornerRadius: 14).fill(hovering ? Color.accentColor.opacity(0.08) : Color.clear))
            VStack(spacing: 8) {
                Image(systemName: "waveform.circle")
                    .font(.system(size: 42, weight: .light))
                Text("Glisse ici un ou plusieurs MKV Dolby Atmos")
                    .font(.headline)
                Text("ou clique pour choisir un fichier")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }
        }
        .frame(height: 150)
        .contentShape(Rectangle())
        .onTapGesture { choose() }
        .onDrop(of: [UTType.fileURL], isTargeted: $hovering) { providers in
            for prov in providers {
                prov.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, _ in
                    var url: URL?
                    if let d = item as? Data { url = URL(dataRepresentation: d, relativeTo: nil) }
                    else if let u = item as? URL { url = u }
                    if let url { DispatchQueue.main.async { conv.add([url]) } }
                }
            }
            return true
        }
    }

    private func choose() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        if let mkv = UTType(filenameExtension: "mkv") { panel.allowedContentTypes = [mkv] }
        if panel.runModal() == .OK { conv.add(panel.urls) }
    }
}

struct JobRow: View {
    @ObservedObject var job: Job

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                icon
                Text(job.input.lastPathComponent).font(.headline).lineLimit(1).truncationMode(.middle)
                Spacer()
                if job.state == .done, let dir = job.outputDir {
                    Button("Afficher dans le Finder") {
                        NSWorkspace.shared.activateFileViewerSelecting([job.outputs.first ?? dir])
                    }
                }
            }
            switch job.state {
            case .waiting:
                Text("En attente").font(.caption).foregroundColor(.secondary)
            case .running:
                ProgressView(value: job.overall)
                HStack {
                    Text("\(Int((job.overall * 100).rounded())) %").font(.caption.monospacedDigit().bold())
                    Text(job.step).font(.caption).foregroundColor(.secondary).lineLimit(1)
                    Spacer()
                    Text(job.remaining).font(.caption.monospacedDigit()).foregroundColor(.secondary)
                }
            case .done:
                Text("\(job.outputs.count) fichier(s) créé(s) dans « \(job.outputDir?.lastPathComponent ?? "") »")
                    .font(.caption).foregroundColor(.secondary)
            case .failed(let msg):
                Text(msg).font(.caption).foregroundColor(.red).textSelection(.enabled)
            }
            ForEach(Array(job.notes.enumerated()), id: \.offset) { _, n in
                Text(n).font(.caption2).foregroundColor(n.hasPrefix("attention") ? .orange : .secondary)
            }
            LogView(job: job)
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder var icon: some View {
        switch job.state {
        case .waiting: Image(systemName: "clock").foregroundColor(.secondary)
        case .running: Image(systemName: "gearshape.2").foregroundColor(.accentColor)
        case .done: Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
        case .failed: Image(systemName: "exclamationmark.triangle.fill").foregroundColor(.red)
        }
    }
}

struct LogView: View {
    @ObservedObject var job: Job
    @State private var open = false

    var body: some View {
        DisclosureGroup(isExpanded: $open) {
            VStack(alignment: .trailing, spacing: 4) {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(alignment: .leading, spacing: 1) {
                            ForEach(Array(job.log.enumerated()), id: \.offset) { i, line in
                                Text(line)
                                    .font(.system(size: 10, design: .monospaced))
                                    .foregroundColor(line.contains("ERREUR") || line.contains("ÉCHEC") ? .red : .primary)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .id(i)
                            }
                        }
                        .textSelection(.enabled)
                        .padding(6)
                    }
                    .frame(height: 140)
                    .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .textBackgroundColor)))
                    .onChange(of: job.log.count) { n in
                        withAnimation { proxy.scrollTo(n - 1, anchor: .bottom) }
                    }
                }
                Button("Copier le journal") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(job.log.joined(separator: "\n"), forType: .string)
                }
                .font(.caption)
            }
        } label: {
            Text("Journal (\(job.log.count) lignes)").font(.caption)
        }
        .onChange(of: job.state) { st in
            if case .failed = st { open = true }
        }
    }
}

struct ContentView: View {
    @StateObject var conv = Converter()

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            DropZone(conv: conv)

            GroupBox("Fichiers à créer") {
                VStack(alignment: .leading, spacing: 6) {
                    Toggle("ADM BWF (objets Atmos, pour Virtuoso, Pro Tools, Dolby Renderer)", isOn: $conv.makeADM)
                    Toggle("WAV 7.1.4 (rendu canaux, se lit partout)", isOn: $conv.make714)
                    Toggle("Un fichier par chapitre (un morceau = un fichier, .atmos compris)", isOn: $conv.splitChapters)
                    Toggle("Master Dolby .atmos (DAMF) : s'ouvre dans le Dolby Renderer", isOn: $conv.keepDAMF)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(4)
            }
            .disabled(conv.busy)

            if conv.jobs.isEmpty {
                Text("Les fichiers sont créés dans un dossier « … - Atmos » à côté de chaque MKV.")
                    .font(.caption).foregroundColor(.secondary)
                Spacer()
            } else {
                List(conv.jobs) { JobRow(job: $0) }
                    .frame(minHeight: 160)
                HStack {
                    if conv.busy {
                        Button("Annuler la conversion en cours") { conv.cancel() }
                    }
                    Spacer()
                    Button("Effacer les terminés") { conv.clearFinished() }.disabled(conv.busy && conv.jobs.count == 1)
                }
            }
        }
        .padding(18)
        .frame(minWidth: 560, minHeight: 520)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@main
struct MkvToAtmosApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate

    var body: some Scene {
        WindowGroup("MKV → Atmos") {
            ContentView()
        }
        .windowResizability(.contentMinSize)
    }
}
