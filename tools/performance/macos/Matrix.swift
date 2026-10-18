// Orchestration for the frozen Q51 corpus. All editor operations remain native
// input; fixture generation/copying happens before measurement begins.
import AppKit
import ApplicationServices
import Foundation

let manifestHash = "0e95e7288b26e37248899dedd09fe5c311866febc3d94d72f550bffbc28b0292"

func localPath(_ path: String) throws {
    let url=URL(fileURLWithPath:path)
    guard path.hasPrefix("/"), url.standardizedFileURL.path==path,
          url.resolvingSymlinksInPath().path==path, path != "/",
          !path.lowercased().contains("onedrive"),
          !path.contains("/Library/CloudStorage/"), !path.contains("/Library/Mobile Documents/") else {
        throw Failure("Use a canonical local path outside cloud-sync and symbolic-link directories")
    }
}
func relativePath(_ path: String) throws {
    guard !path.isEmpty, !path.hasPrefix("/"),
          !path.split(separator:"/",omittingEmptySubsequences:false).contains(where:{$0.isEmpty || $0=="." || $0==".."}) else {
        throw Failure("Unsafe relative evidence path")
    }
}
func regularFiles(_ root: String) throws -> Set<String> {
    var names=Set<String>(), enumerationError: Error?
    guard let walk=files.enumerator(at:URL(fileURLWithPath:root),includingPropertiesForKeys:nil,
                                   errorHandler:{_,error in enumerationError=error; return false}) else {
        throw Failure("Cannot enumerate directory")
    }
    for case let url as URL in walk {
        let attributes=try files.attributesOfItem(atPath:url.path)
        let type=attributes[.type] as? FileAttributeType
        guard type == .typeDirectory || type == .typeRegular else {throw Failure("Non-regular entry in evidence tree")}
        if type == .typeRegular {names.insert(String(url.path.dropFirst(root.count+1)))}
    }
    if let enumerationError {throw enumerationError}
    return names
}
func prepare(corpus: String, destination: String) throws {
    try localPath(corpus); try ownedRoot(corpus); try localPath(destination)
    guard !destination.hasPrefix(corpus+"/"), try hash(corpus+"/manifest.json")==manifestHash else {
        throw Failure("Only the frozen full corpus may be copied to a separate run root")
    }
    let manifest=try read(corpus+"/manifest.json")
    let records=(manifest["files"] as? [Object] ?? []).filter{text($0["path"]).hasPrefix("library/")}
    guard records.count==1010 else {throw Failure("Expected 1000 notes and 10 notebooks")}
    var expected=Set<String>()
    for record in records {
        let path=text(record["path"]); try relativePath(path)
        expected.insert(String(path.dropFirst("library/".count)))
    }
    guard try regularFiles(corpus+"/library")==expected else {throw Failure("Corpus file set differs from manifest")}
    try directory(destination)
    // FileManager creates independent files (APFS clones are allowed); never
    // request a hard link. Verify identities and bytes before granting ownership.
    try files.copyItem(atPath:corpus+"/library",toPath:destination+"/library")
    for record in records {
        let path=text(record["path"]), source=corpus+"/"+path, copied=destination+"/"+path
        let before=try files.attributesOfItem(atPath:source), after=try files.attributesOfItem(atPath:copied)
        guard try hash(copied)==text(record["sha256"]), number(after[.size])==number(record["bytes"]),
              !(number(before[.systemNumber])==number(after[.systemNumber]) && number(before[.systemFileNumber])==number(after[.systemFileNumber])) else {
            throw Failure("Independent copy/hash verification failed: "+path)
        }
    }
    guard try regularFiles(destination+"/library")==expected else {throw Failure("Copied file set differs")}
    try createExclusive(destination+"/.cuenote-performance",Data(marker.utf8))
    try write(destination+"/corpus-copy.json",["manifestSha256":manifestHash,"files":records.count,"hardLinks":false])
}
func subprocess(_ executable: String, _ args: [String], log: String, environment: [String:String]? = nil) throws {
    guard !files.fileExists(atPath:log) else {throw Failure("Existing process log")}
    try createExclusive(log,Data())
    let output=try FileHandle(forWritingTo:URL(fileURLWithPath:log)); defer {try? output.close()}
    let process=Process(); process.executableURL=URL(fileURLWithPath:executable); process.arguments=args
    if let environment {process.environment=environment}
    process.standardOutput=output; process.standardError=output
    try process.run(); process.waitUntilExit()
    guard process.terminationReason == .exit && process.terminationStatus==0 else {
        throw Failure("Native tool failed; preserve "+log)
    }
}

func resources(kit: String, base: String) throws {
    try localPath(kit); try localPath(base); try directory(base)
    try createExclusive(base+"/.cuenote-resource-tests",Data("CueNote isolated resource tests v1\n".utf8))
    let receipt=try read(kit+"/receipt.json"), binaries=try object(receipt["binaries"])
    let suites=["format_v1","image_content","rich_content","storage"]
    guard (receipt["resourceSuites"] as? [String])==suites else {throw Failure("Missing frozen resource suites")}
    var env=ProcessInfo.processInfo.environment
    env["CUENOTE_ACCEPTANCE_TEST_ROOT"]=base
    for key in Array(env.keys) where key.hasPrefix("CUENOTE_FAULT_") || key=="CUENOTE_TEST_ROOT" {env.removeValue(forKey:key)}
    var results=[Object]()
    do {
        for suite in suites {
            let file="resource-tests/"+suite, actual=try hash(kit+"/"+file)
            guard actual==text(binaries[file]) else {throw Failure("Resource suite hash mismatch")}
            try subprocess(kit+"/"+file,["--test-threads=1"],log:base+"/"+suite+".log",environment:env)
            let log=try String(contentsOfFile:base+"/"+suite+".log",encoding:.utf8)
            guard log.contains("test result: ok."), log.contains("; 0 failed; 0 ignored;") else {throw Failure("Tests did not all pass")}
            results.append(["suite":suite,"sha256":actual,"logSha256":try hash(base+"/"+suite+".log"),"passed":true])
        }
        try write(base+"/results.json",["sourceCommit":receipt["sourceCommit"] ?? "", "os":ProcessInfo.processInfo.operatingSystemVersionString,
            "suites":results,"passed":true,"hostedCI":env["RUNNER_ENVIRONMENT"]=="github-hosted",
            "scope":"Native Q52 format/image/storage regressions on this host; no UI, OneDrive or power-loss claim"])
    } catch {
        try? write(base+"/results.json",["suites":results,"passed":false,"error":String(describing:error)])
        throw error
    }
}
func captureCommand(_ executable: String, _ args: [String]) throws -> String {
    let process=Process(), output=Pipe()
    process.executableURL=URL(fileURLWithPath:executable); process.arguments=args; process.standardOutput=output
    try process.run()
    let data=output.fileHandleForReading.readDataToEndOfFile(); process.waitUntilExit()
    guard process.terminationReason == .exit && process.terminationStatus==0 else {throw Failure("Environment query failed")}
    return String(decoding:data,as:UTF8.self).trimmingCharacters(in:.whitespacesAndNewlines)
}
func jsonLines(_ path: String) throws -> [Object] {
    if !files.fileExists(atPath:path) {return []}
    return try String(contentsOfFile:path,encoding:.utf8).split(separator:"\n").map {
        try object(JSONSerialization.jsonObject(with:Data($0.utf8)))
    }
}
let thresholds: [(String,Int,Double,Bool)] = [
    ("startup",10,3000,true),("initial-index",3,60000,true),
    ("ordinary-open",60,1000,false),("stress-open",40,5000,false),
    ("ordinary-save",32,2000,false),("stress-save",32,10000,false),
    ("search",30,300,false),("ordinary-map",30,1000,false),("stress-map",30,3000,false),
    ("ordinary-input",100,100,false),("stress-input",100,200,false)
]
func distribution(_ rows: [Object]) -> Object {
    let values=rows.map{number($0["elapsedMs"])}.sorted()
    guard !values.isEmpty else {return ["count":0]}
    return ["count":values.count,"p95Ms":values[Int(ceil(Double(values.count)*0.95))-1],"maxMs":values.last!]
}
func summarize(base: String, runs: [String], sha: String, completed: Bool) throws {
    var samples=[Object](), failures=[Object](), closed=true
    for run in runs {
        let launcher=try read(run+"/launcher.json")
        guard text(launcher["sha256"])==sha, !yes(launcher["traceEnabled"]) else {throw Failure("Mixed binary or trace-enabled run")}
        let root=text(launcher["root"]), corpus=try read(root+"/corpus-copy.json")
        guard text(corpus["manifestSha256"])==manifestHash else {throw Failure("Mixed corpus")}
        var startup=try read(run+"/startup.json"); startup["run"]=run
        if text(startup["operation"])=="startup" {startup["metric"]="startup"; startup["cacheState"]="indexed"; samples.append(startup)}
        for var row in try jsonLines(run+"/measurements.jsonl") {
            guard number(row["elapsedMs"]).isFinite && number(row["elapsedMs"])>=0 else {throw Failure("Invalid duration")}
            row["run"]=run
            if !text(row["metric"]).isEmpty {samples.append(row)}
        }
        failures += try jsonLines(run+"/failures.jsonl")
        let exit=try? read(run+"/closed.json")
        closed = closed && (exit.map{yes($0["normalExit"]) && yes($0["exitStatusAvailable"])} == true)
    }
    failures += try jsonLines(base+"/failures.jsonl")
    var metrics=[Object](), allPassed=completed && failures.isEmpty && closed
    for (name,count,limit,useMax) in thresholds {
        let rows=samples.filter{text($0["metric"])==name}
        var result=distribution(rows)
        let measured=number(result[useMax ? "maxMs":"p95Ms"])
        let passed=rows.count>=count && measured<=limit
        result["metric"]=name; result["minimumCount"]=count; result["limitMs"]=limit
        result["rule"]=useMax ? "maximum":"nearest-rank p95"; result["passed"]=passed
        var groups=Object()
        for group in Set(rows.map{text($0["cacheState"])}).sorted() {
            groups[group]=distribution(rows.filter{text($0["cacheState"])==group})
        }
        result["groups"]=groups; metrics.append(result); allPassed = allPassed && passed
    }
    try write(base+"/summary.json",["format":1,"manifestSha256":manifestHash,"binarySha256":sha,
        "completed":completed,"allRunsClosed":closed,"timingGatesPassed":allPassed,"metrics":metrics,
        "rawSamples":samples,"failures":failures,
        "scope":"Q51 native timing evidence only. Check content verification, feedback, input calibration, Q52 and device/OneDrive cases separately."])
}
func matrix(kit: String, base: String) throws {
    guard ProcessInfo.processInfo.environment["CI"] == nil,
          ProcessInfo.processInfo.environment["RUNNER_ENVIRONMENT"] != "github-hosted" else {
        throw Failure("The real-device matrix must not run as hosted-CI acceptance")
    }
    guard AXIsProcessTrusted(), CGPreflightPostEventAccess() else {
        throw Failure("Grant Accessibility to this driver/Terminal, then retry. No inputs or samples were created.")
    }
    try localPath(kit); try localPath(base)
    let receipt=try read(kit+"/receipt.json"), binaries=try object(receipt["binaries"])
    guard text(receipt["applicationIdentifier"])=="com.hobocy.cuenote.performance",
          text(receipt["manifestSha256"])==manifestHash, !yes(receipt["sourceDirty"]),
          try hash(kit+"/manifest.json")==manifestHash else {throw Failure("Unexpected kit receipt")}
    for (relative, expected) in binaries {
        try relativePath(relative)
        guard try hash(kit+"/"+relative)==text(expected) else {throw Failure("Kit binary hash mismatch: "+relative)}
    }
    let plans=try object(receipt["plans"])
    guard !plans.isEmpty else {throw Failure("Missing action-plan hashes")}
    for (relative,expected) in plans {
        try relativePath(relative)
        guard try hash(kit+"/plans/"+relative)==text(expected) else {throw Failure("Action plan changed: "+relative)}
    }
    let attributes=try files.attributesOfFileSystem(forPath:URL(fileURLWithPath:base).deletingLastPathComponent().path)
    guard number(attributes[.systemFreeSize])>=20*1024*1024*1024 else {throw Failure("At least 20 GiB free local space is required")}
    try directory(base)
    let exe=kit+"/CueNote Performance.app/Contents/MacOS/cuenote", sha=try hash(exe)
    let fixture=kit+"/performance_fixture", corpus=base+"/corpus"
    var runs=[String](), active: Driver?
    do {
        try write(base+"/environment.json",["os":ProcessInfo.processInfo.operatingSystemVersionString,
            "cpu":try captureCommand("/usr/sbin/sysctl",["-n","machdep.cpu.brand_string"]),
            "memoryBytes":ProcessInfo.processInfo.physicalMemory,
            "screens":NSScreen.screens.map{["x":$0.frame.origin.x,"y":$0.frame.origin.y,"width":$0.frame.width,"height":$0.frame.height,"backingScale":$0.backingScaleFactor]},
            "freeBytes":number(attributes[.systemFreeSize]),"receipt":receipt,
            "startedUtc":ISO8601DateFormatter().string(from:Date()),
            "note":"No OS-cache clearing; first means first opening within this matrix. Automatic backups remain enabled."])
        print("Generating and verifying the frozen corpus before any measurement...")
        try subprocess(fixture,["generate",corpus],log:base+"/generate.log")
        guard try hash(corpus+"/manifest.json")==manifestHash else {throw Failure("Generated corpus differs")}
        try subprocess(fixture,["verify",corpus],log:base+"/verify.log")
        try prepare(corpus:corpus,destination:base+"/calibration")
        // Validate native Unicode input, complete selections and actual scroll
        // movement on a disposable copy before collecting acceptance samples.
        let calibrationRun=try launch([base+"/calibration",exe,sha,"welcome"])
        let calibration=try Driver(run:calibrationRun); active=calibration
        try calibration.actions(kit+"/plans/index.json")
        try calibration.actions(kit+"/plans/ordinary-select.json")
        try calibrate(calibration,kit:kit)
        try calibration.close(); active=nil
        try write(base+"/calibration.json",["run":calibrationRun,"nativeInputVerified":true,
            "scope":"Current window position and display configuration only; no CI input acceptance"])
        for index in 1...3 {try prepare(corpus:corpus,destination:base+"/index-\(index)")}
        for index in 1...3 {
            let run=try launch([base+"/index-\(index)",exe,sha,"welcome"]); runs.append(run)
            let driver=try Driver(run:run); active=driver
            try driver.actions(kit+"/plans/index.json"); try driver.close(); active=nil
        }
        let run=try launch([base+"/index-1",exe,sha,"index"]); runs.append(run)
        let driver=try Driver(run:run); active=driver
        let sequence=["ordinary-opens","ordinary-select","ordinary-saves","ordinary-inputs","ordinary-maps",
                      "stress-opens","stress-select","stress-saves","stress-inputs","stress-maps",
                      "search","clear-search","cancel-save-wait","ordinary-select"]
        for plan in sequence {
            var timing=Object()
            if yes(try driver.snapshot()["editable"]) {_ = try driver.until(["kind":"save"],timings:&timing)}
            try driver.actions(kit+"/plans/"+plan+".json")
        }
        try driver.close(); active=nil
        let manifest=try read(corpus+"/manifest.json")
        guard let ordinary=(manifest["notes"] as? [Object])?.first(where:{text($0["collection"])=="library" && text($0["profile"])=="ordinary"}) else {throw Failure("Missing ordinary note")}
        for _ in 0..<10 {
            let restart=try launch([base+"/index-1",exe,sha,"editor-first-image",text(ordinary["noteId"])]); runs.append(restart)
            active=try Driver(run:restart); try active!.close(); active=nil
        }
        // Native package comparison runs only after all timing/quit samples.
        try subprocess(fixture,["verify-edited",corpus,base+"/index-1"],log:base+"/content-verification.log")
        try write(base+"/runs.json",runs)
        try summarize(base:base,runs:runs,sha:sha,completed:true)
        print("Matrix finished. Keep the entire directory: "+base)
    } catch {
        try? append(base+"/failures.jsonl",["error":String(describing:error),"activeRun":active?.run ?? ""])
        if let active {
            do {try active.close()} catch {try? append(base+"/failures.jsonl",["normalCloseError":String(describing:error)])}
        }
        try? write(base+"/runs.json",runs)
        try? summarize(base:base,runs:runs,sha:sha,completed:false)
        throw error
    }
}

func calibrate(_ driver: Driver, kit: String) throws {
    var longest=""
    for file in try files.contentsOfDirectory(atPath:kit+"/plans").filter({$0.hasSuffix(".json")}) {
        let steps=try JSONSerialization.jsonObject(with:Data(contentsOf:URL(fileURLWithPath:kit+"/plans/"+file))) as? [Object] ?? []
        for step in steps {
            for action in (step["setup"] as? [Object] ?? []) + (step["actions"] as? [Object] ?? []) {
                if text(action["kind"])=="text" && text(action["text"]).utf16.count>longest.utf16.count {longest=text(action["text"])}
            }
        }
    }
    guard !longest.isEmpty else {throw Failure("No frozen Unicode input available for calibration")}
    var timing=Object()
    let title="#note-title-input", heading="[data-testid=\"rich-editor-input-content\"] h1"
    try driver.scroll(title)
    let titleGeometry=try driver.request("element",selector:title)
    try driver.action(["kind":"click","selector":title])
    try driver.action(["kind":"key","code":"KeyA","modifiers":2])
    let titleSelection=try driver.until(["kind":"title-selection"],timeout:5000,timings:&timing)
    try append(driver.run+"/calibration-steps.jsonl",["step":"title-selection","state":titleSelection])
    try driver.action(["kind":"text","text":longest])
    let titleResult=try driver.until(["kind":"dirty-title","text":longest],timeout:5000,timings:&timing)
    try driver.action(["kind":"key","code":"KeyS","modifiers":2])
    let titleSaved=try driver.until(["kind":"save"],timings:&timing)
    try append(driver.run+"/calibration-steps.jsonl",["step":"title-saved","state":titleSaved])
    try driver.scroll(heading); try driver.action(["kind":"click","selector":heading])
    try driver.action(["kind":"key","code":"Home"])
    try driver.action(["kind":"key","code":"End","modifiers":8])
    let selection=try driver.until(["kind":"content-heading-selection"],timeout:5000,timings:&timing)
    try append(driver.run+"/calibration-steps.jsonl",["step":"heading-selection","state":selection])
    try driver.action(["kind":"text","text":longest])
    let content=try driver.until(["kind":"content-heading","text":longest],timeout:5000,timings:&timing)
    try driver.action(["kind":"key","code":"KeyS","modifiers":2])
    _ = try driver.until(["kind":"save"],timings:&timing)
    let target="[data-testid=\"rich-editor-input-content\"] > :last-child"
    let before=try driver.request("element",selector:target)
    try driver.scroll(target)
    let after=try driver.request("element",selector:target)
    let beforeValue=try object(before["value"]), afterValue=try object(after["value"])
    let oldPositions=(beforeValue["containers"] as? [Object] ?? []).map{number($0["scrollTop"])}
    let positions=(afterValue["containers"] as? [Object] ?? []).map{number($0["scrollTop"])}
    let oldDocument=try object(beforeValue["documentScroll"]), document=try object(afterValue["documentScroll"])
    let documentMoved=number(document["top"]) != number(oldDocument["top"]) || number(document["left"]) != number(oldDocument["left"])
    guard yes(afterValue["visible"]), positions != oldPositions || documentMoved else {throw Failure("Native calibration did not prove actual scroll movement")}
    try write(driver.run+"/calibration.json",["longestInput":longest,"utf16Length":longest.utf16.count,
        "titleGeometry":titleGeometry,"titleSelection":titleSelection,"titleResult":titleResult,"headingSelection":selection,
        "contentResult":content,"scrollBefore":before,"scrollAfter":after,
        "scope":"Native events and read-only observations, current display/window geometry"])
    try driver.capture("calibrated")
}
