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
    var outputDir: URL?

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
        let out = Pipe()
        p.standardOutput = out
        p.standardError = Pipe()
        process = p

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
            DispatchQueue.main.async {
                guard let self else { return }
                if case .failed = job.state {
                } else if proc.terminationReason == .uncaughtSignal {
                    job.state = .failed("Conversion annulée.")
                } else if proc.terminationStatus == 0 {
                    job.state = .done
                    job.step = "Terminé"
                    job.progress = 1
                } else {
                    job.state = .failed("La conversion a échoué (code \(proc.terminationStatus)).")
                }
                self.busy = false
                self.process = nil
                self.runNext()
            }
        }
        do {
            try p.run()
        } catch {
            job.state = .failed("Impossible de lancer la conversion : \(error.localizedDescription)")
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
        case "info":
            if let m = obj["message"] as? String { job.notes.append(m) }
        case "file":
            if let p = obj["path"] as? String { job.outputs.append(URL(fileURLWithPath: p)) }
        case "error":
            job.state = .failed(obj["message"] as? String ?? "Erreur inconnue")
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
            case .running, .waiting:
                ProgressView(value: job.progress) { Text(job.step).font(.caption) }
            case .done:
                Text("\(job.outputs.count) fichier(s) créé(s) dans « \(job.outputDir?.lastPathComponent ?? "") »")
                    .font(.caption).foregroundColor(.secondary)
            case .failed(let msg):
                Text(msg).font(.caption).foregroundColor(.red).textSelection(.enabled)
            }
            ForEach(Array(job.notes.enumerated()), id: \.offset) { _, n in
                Text(n).font(.caption2).foregroundColor(n.hasPrefix("attention") ? .orange : .secondary)
            }
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

struct ContentView: View {
    @StateObject var conv = Converter()

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            DropZone(conv: conv)

            GroupBox("Fichiers à créer") {
                VStack(alignment: .leading, spacing: 6) {
                    Toggle("ADM BWF (objets Atmos, pour Virtuoso, Pro Tools, Dolby Renderer)", isOn: $conv.makeADM)
                    Toggle("WAV 7.1.4 (rendu canaux, se lit partout)", isOn: $conv.make714)
                    Toggle("Un fichier par chapitre (un morceau = un fichier)", isOn: $conv.splitChapters)
                    Toggle("Garder aussi le master Dolby brut (DAMF)", isOn: $conv.keepDAMF)
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
